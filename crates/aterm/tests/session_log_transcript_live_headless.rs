// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless check of session log phase 3B's transcript steps (plan §2,
//! rulings S1, S6, S30), driven through the one `aterm` binary against a real
//! headless instance and its real control socket.
//!
//! * A CLAUDE-SHAPED STAND-IN: a POSIX `sh` script run as `claude` via
//!   `exec -a`, with `CLAUDE_CONFIG_DIR` in its own environment (the lifted
//!   Claude home, addendum A25). On macOS the interpreter is a copy of
//!   `/bin/bash` signed ad hoc: the kernel hides a platform binary's
//!   environment from another process, so `/bin/sh` itself would hide the
//!   home the footer reads. It writes its registry row under that home and
//!   draws Claude Code's idle screen, or its busy one (ticking) while the
//!   test's `busy` file exists. The test writes the transcript itself:
//!   synthetic rows in Claude Code's shape, every value made up.
//! * HISTORY IS NEVER REPLAYED (S1): the transcript holds 20 turns before the
//!   stand-in starts; the log says so once (`source field=transcript …
//!   skipped=<its length>`) and holds no turn of them.
//! * A LIVE TURN: the screen's busy opens the turn, the transcript's prompt
//!   names it (`amend … vref=`, exactly one turn per vref, S30), its Bash
//!   step opens and closes (`await log kind=tool` returns), and the idle
//!   screen's settle closes the turn (`await log close=turn` returns).
//! * THE CLAUSE (plan §5, rulings S11; `[session_log] clause = true` and the
//!   in-grid strip, whose menu `open tab-menu` reads headless): the tab's
//!   menu says `now: running shell` while the step is open, and a `ctl`
//!   client's label shows in `log`, never in the menu.
//! * NO TRANSCRIPT TEXT (S6, D5 decision B): no prompt, reply, tool input or
//!   result word reaches `log` at either tier, `brief`, the menu, or any file
//!   under the instance's scratch world outside the Claude home.
//!
//! ISOLATION: scratch HOME/XDG roots, a private control socket, every
//! automatic lane off, `--no-reroute`, `SHELL=/bin/sh`
//! (`support/launch_isolation.rs`). A headless instance opens no window.

#![cfg(unix)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aterm_agent::supervise::prompt::fixtures::composer;

use crate::headless_boot;

use headless_boot::{Instance, boot_session, ctl_ok, field, process_tree, status, type_line};

/// The registry row's session id: the transcript's file stem.
const SESSION_ID: &str = "00000000-0000-4000-8000-0000000003b0";

/// The words the transcript's text carries: none may leave it.
const SECRETS: &[&str] = &[
    "QZHISTORYPROMPT",
    "QZHISTORYINPUT",
    "QZLIVEPROMPT",
    "QZLIVEINPUT",
    "QZLIVEREPLY",
    "QZLIVERESULT",
];

/// The hang detector (AGENTS.md): a wait for what must happen.
const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;
const HANG_MS: &str = "timeout=60000";

/// The stand-in (module doc): `$1` the screens' directory (`ready.scr`,
/// `busy.scr`, and the `busy` flag), `$2` its working directory, `$3` the
/// session id. Answers `--version` at once.
const STANDIN: &str = r#"#!/bin/sh
[ "$1" = --version ] && { echo 2.1.300; exit 0; }
dir="$1"; work="$2"; sid="$3"
cd "$work" || exit 3
stty -echo 2>/dev/null
printf '{"pid":%s,"sessionId":"%s","cwd":"%s","startedAt":%s000,"version":"2.1.300"}' \
  "$$" "$sid" "$work" "$(date +%s)" > "$CLAUDE_CONFIG_DIR/sessions/$$.json"
cur=""; n=0
while :; do
  if [ -e "$dir/busy" ]; then want=busy; else want=ready; fi
  if [ "$want" != "$cur" ]; then
    cur="$want"; printf '\033[H\033[2J'; cat "$dir/$want.scr"; printf '\033[6;3H'
  fi
  if [ "$cur" = busy ]; then
    n=$((n + 1)); printf '\0337\033[3;1H\033[2K✶ Deliberating… (%ss · esc to interrupt)\0338' "$n"
  fi
  sleep 0.3
done
"#;

/// Rows as the stand-in draws them, `\r\n` between (the stand-in puts the
/// cursor on the caret row, the sixth, after them).
fn render(rows: &[String]) -> String {
    rows.join("\r\n")
}

/// Claude Code at rest after a turn.
fn ready() -> Vec<String> {
    let mut r = vec![
        "⏺ Ready.".to_string(),
        String::new(),
        "✻ Worked for 3s · done 4:24 PM".to_string(),
        String::new(),
    ];
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

/// Claude Code mid-turn (its spinner row ticked by the stand-in).
fn busy() -> Vec<String> {
    let mut r = vec![
        "⏺ On it.".to_string(),
        String::new(),
        "✶ Deliberating… (1s · esc to interrupt)".to_string(),
        String::new(),
    ];
    r.extend(composer("  ⏵⏵ bypass permissions on · esc to interrupt"));
    r
}

/// The stand-in's interpreter under `root`: on macOS a copy of `/bin/bash`
/// signed ad hoc (module doc, `aterm_uds::deadline::ad_hoc_copy`), elsewhere
/// `/bin/sh`.
fn interpreter(root: &Path) -> PathBuf {
    if !cfg!(target_os = "macos") {
        return PathBuf::from("/bin/sh");
    }
    let copy = root.join("bin").join("claude-sh");
    std::fs::create_dir_all(copy.parent().expect("a parent")).expect("bin dir");
    aterm_uds::deadline::ad_hoc_copy(Path::new("/bin/bash"), &copy);
    copy
}

/// The tab menu's rows as `controls tab-menu` reads them after `open
/// tab-menu`, the card closed again.
fn menu(inst: &Instance) -> Vec<String> {
    ctl_ok(inst, &["open", "tab-menu"]);
    let rows = ctl_ok(inst, &["controls", "tab-menu"])
        .lines()
        .filter_map(|l| {
            l.split_once(" text=")
                .map(|(_, t)| t.trim_matches('"').to_string())
        })
        .collect();
    ctl_ok(inst, &["key", "escape"]);
    rows
}

/// Wall ms now.
fn wall_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("after the epoch")
            .as_millis(),
    )
    .expect("ms fit")
}

/// Claude Code's RFC 3339 spelling of wall ms `ms`.
fn iso(ms: u64) -> String {
    let mut s = aterm_types::rfc3339::format_rfc3339(ms / 1000);
    s.pop();
    format!("{s}.{:03}Z", ms % 1000)
}

fn prompt(id: &str, ms: u64, text: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"promptId":"{id}","type":"user","message":{{"role":"user","content":"{text}"}},"uuid":"u-{id}","timestamp":"{}","version":"2.1.300"}}"#,
        iso(ms)
    )
}

fn call(msg: &str, ms: u64, tool_id: &str, input: &str, reply: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"assistant","message":{{"id":"{msg}","model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":"{reply}"}},{{"type":"tool_use","id":"{tool_id}","name":"Bash","input":{{"command":"{input}"}}}}],"usage":{{"input_tokens":3,"output_tokens":20,"cache_creation_input_tokens":100,"cache_read_input_tokens":1000}}}},"uuid":"a-{msg}","timestamp":"{}","version":"2.1.300"}}"#,
        iso(ms)
    )
}

fn result(ms: u64, tool_id: &str, text: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{tool_id}","is_error":false,"content":[{{"type":"text","text":"{text}"}}]}}]}},"uuid":"r-{tool_id}","timestamp":"{}","toolUseResult":{{"stdout":"{text}"}}}}"#,
        iso(ms)
    )
}

fn turn_end(ms: u64, dur: u64) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":{dur},"timestamp":"{}","uuid":"d-{ms}","isMeta":false}}"#,
        iso(ms)
    )
}

fn append(path: &Path, rows: &[String]) {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open the transcript");
    for row in rows {
        writeln!(f, "{row}").expect("append a row");
    }
}

/// `log`'s records for `sid` at `tier` (`host` or `content`).
fn log(inst: &Instance, sid: &str, tier: &str) -> Vec<String> {
    ctl_ok(
        inst,
        &[&format!("@{sid}"), "log", &format!("tier={tier}"), "n=400"],
    )
    .lines()
    .map(str::to_string)
    .collect()
}

/// The reader's cursor: `(log_seq, log_ep)` off `status`.
fn cursor(inst: &Instance, sid: &str) -> (String, String) {
    let st = status(inst, sid);
    (
        field(&st, "log_seq").expect("log_seq").to_string(),
        field(&st, "log_ep").expect("log_ep").to_string(),
    )
}

/// `await log <what>` from the cursor `at`, a hang detector: its answer.
fn await_log(inst: &Instance, sid: &str, what: &str, at: &(String, String)) -> String {
    let reply = ctl_ok(
        inst,
        &[
            &format!("@{sid}"),
            "await",
            "log",
            what,
            &format!("since={}", at.0),
            &format!("ep={}", at.1),
            HANG_MS,
        ],
    );
    assert!(
        reply.starts_with("OK seq="),
        "await log {what}: {reply}\nlog:\n{}",
        log(inst, sid, "host").join("\n")
    );
    reply
}

fn having<'a>(lines: &'a [String], needle: &str) -> Vec<&'a String> {
    lines.iter().filter(|l| l.contains(needle)).collect()
}

/// Every file under `root` but those under `skip`, read whole.
fn files_under(root: &Path, skip: &[PathBuf], out: &mut Vec<(PathBuf, Vec<u8>)>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if skip.iter().any(|s| path.starts_with(s)) {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            files_under(&path, skip, out);
        } else if kind.is_file()
            && let Ok(bytes) = std::fs::read(&path)
        {
            out.push((path, bytes));
        }
    }
}

/// The module doc, end to end.
#[test]
fn session_log_transcript_live_headless() {
    let inst = headless_boot::boot_with(
        "atslt",
        |root, cmd| {
            // The in-grid strip (its menu is what `open tab-menu` reads
            // headless) and the clause, at the config's top and its end.
            let path = root.join("cfg/aterm/aterm.toml");
            let base = std::fs::read_to_string(&path).expect("the scratch config");
            std::fs::write(
                &path,
                format!("tab_strip_rows = 1\n{base}[session_log]\nclause = true\n"),
            )
            .expect("write the scratch config");
            cmd.args(["--lines", "40", "--columns", "120"]);
        },
        |_| true,
    );
    let (_, sid) = boot_session(&inst);
    let dir = inst.tmp.join("screens");
    let work = inst.tmp.join("work");
    let claude = inst.tmp.join("claude");
    for d in [&dir, &work, &claude.join("sessions")] {
        std::fs::create_dir_all(d).expect("a scratch dir");
    }
    std::fs::write(dir.join("ready.scr"), render(&ready())).expect("ready.scr");
    std::fs::write(dir.join("busy.scr"), render(&busy())).expect("busy.scr");
    let script = inst.tmp.join("claude.sh");
    std::fs::write(&script, STANDIN).expect("the stand-in");
    let sh = interpreter(&inst.tmp);
    let projects = claude
        .join("projects")
        .join(aterm_agent::harness::footer::project_slug(&work));
    std::fs::create_dir_all(&projects).expect("the project dir");
    let transcript = projects.join(format!("{SESSION_ID}.jsonl"));

    // HISTORY: twenty turns before the stand-in starts.
    let t0 = wall_ms().saturating_sub(3_600_000);
    let mut history = Vec::new();
    for i in 0..20u64 {
        let ms = t0 + i * 60_000;
        let tool = format!("toolu_hist{i:02}");
        history.push(prompt(&format!("vhist{i:02}"), ms, "QZHISTORYPROMPT"));
        history.push(call(
            &format!("mh{i}"),
            ms + 1_000,
            &tool,
            "QZHISTORYINPUT",
            "ok",
        ));
        history.push(result(ms + 2_000, &tool, "fine"));
        history.push(turn_end(ms + 3_000, 2_000));
    }
    append(&transcript, &history);
    let skipped = std::fs::metadata(&transcript)
        .expect("the transcript")
        .len();

    let start = cursor(&inst, &sid);
    type_line(
        &inst,
        &sid,
        &format!(
            "CLAUDE_CONFIG_DIR={} /bin/bash -c 'exec -a claude {} {} {} {} {SESSION_ID}'",
            claude.display(),
            sh.display(),
            script.display(),
            dir.display(),
            work.display()
        ),
    );
    let deadline = Instant::now() + HANG;
    while !status(&inst, &sid).contains(" program=claude ") {
        assert!(
            Instant::now() < deadline,
            "the stand-in never ran as claude: {}\n{}",
            status(&inst, &sid),
            process_tree(&inst)
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // S1: the history said once, as skipped, and no turn of it.
    let source = await_log(&inst, &sid, "kind=source", &start);
    assert!(
        source.contains(" source ") && source.contains(" field=transcript "),
        "{source}"
    );
    assert!(
        source.contains(&format!(" skipped={skipped} "))
            || source.ends_with(&format!(" skipped={skipped}\n")),
        "the whole history is skipped ({skipped} bytes): {source}"
    );
    let got = log(&inst, &sid, "host");
    assert_eq!(
        having(&got, " field=transcript ").len(),
        1,
        "one source mark: {got:#?}"
    );
    assert!(
        having(&got, "vhist").is_empty() && having(&got, "toolu_hist").is_empty(),
        "no past turn is replayed (S1): {got:#?}"
    );

    // A LIVE TURN: busy opens it; its rows name it and its step.
    let before_turn = cursor(&inst, &sid);
    std::fs::write(dir.join("busy"), b"").expect("busy");
    let opened = await_log(&inst, &sid, "open=turn", &before_turn);
    let turn = opened
        .split_whitespace()
        .skip_while(|t| *t != "open")
        .nth(1)
        .expect("the turn's node")
        .to_string();
    // Its rows are dated at the turn's open as the log said it (S30: what
    // holds however long the calls above took, never a latency budget).
    let t_open: u64 = opened
        .split_whitespace()
        .skip_while(|t| *t != "open")
        .find_map(|t| t.strip_prefix("t="))
        .and_then(|t| t.parse().ok())
        .expect("the turn's open names its wall");
    let before_step = cursor(&inst, &sid);
    append(
        &transcript,
        &[
            prompt("vlive01", t_open, "QZLIVEPROMPT"),
            call(
                "ml1",
                t_open + 100,
                "toolu_live01",
                "QZLIVEINPUT",
                "QZLIVEREPLY",
            ),
        ],
    );
    let step = await_log(&inst, &sid, "kind=tool", &before_step);
    assert!(
        step.contains(" open ")
            && step.contains(" vref=toolu_live01 ")
            && step.contains(" cls=shell "),
        "{step}"
    );
    // THE CLAUSE (S11): the open step is the menu's `now:` row; a `ctl`
    // client's label is the log's alone and hides nothing.
    let label = ctl_ok(
        &inst,
        &[
            &format!("@{sid}"),
            "mark",
            "-",
            "label",
            "text=QZCTLLABEL",
            "ttl=60",
        ],
    );
    assert!(label.contains("by=ctl"), "{label}");
    let rows = menu(&inst);
    assert!(
        rows.iter().any(|r| r.starts_with("now: running shell · ")),
        "the step's clause: {rows:#?}"
    );
    assert!(
        !rows.iter().any(|r| r.contains("QZCTLLABEL")),
        "a ctl label is never shown: {rows:#?}"
    );
    assert_eq!(
        having(&log(&inst, &sid, "host"), "QZCTLLABEL").len(),
        1,
        "the label is the log's"
    );
    let before_close = cursor(&inst, &sid);
    let later = wall_ms();
    append(
        &transcript,
        &[
            result(later, "toolu_live01", "QZLIVERESULT"),
            turn_end(later + 100, 1_500),
        ],
    );
    let closed = await_log(&inst, &sid, "kind=tool", &before_close);
    assert!(
        closed.contains(" close ") && closed.contains(" out=ok "),
        "{closed}"
    );
    let before_end = cursor(&inst, &sid);
    std::fs::remove_file(dir.join("busy")).expect("idle");
    let ended = await_log(&inst, &sid, "close=turn", &before_end);
    assert!(ended.contains(&format!(" close {turn} turn ")), "{ended}");

    let got = log(&inst, &sid, "host");
    assert_eq!(
        having(&got, " vref=vlive01 ").len(),
        1,
        "the prompt names its turn once: {got:#?}"
    );
    assert!(
        having(&got, " vref=vlive01 ")[0].contains(&format!(" amend {turn} turn ")),
        "{got:#?}"
    );
    assert_eq!(
        got.iter()
            .filter(|l| l.contains(" open t") && l.contains(" turn "))
            .count(),
        1,
        "exactly one turn (S30): {got:#?}"
    );

    // NO TRANSCRIPT TEXT anywhere aterm keeps or answers.
    let mut answered = log(&inst, &sid, "content");
    answered.extend(got);
    answered.extend(menu(&inst));
    answered.extend(
        ctl_ok(&inst, &[&format!("@{sid}"), "brief"])
            .lines()
            .map(str::to_string),
    );
    for word in SECRETS {
        assert!(
            having(&answered, word).is_empty(),
            "{word} left the transcript: {answered:#?}"
        );
    }
    let mut files = Vec::new();
    files_under(
        &inst.tmp,
        &[claude.clone(), dir.clone(), script.clone(), sh.clone()],
        &mut files,
    );
    for (path, bytes) in &files {
        let text = String::from_utf8_lossy(bytes);
        for word in SECRETS {
            assert!(
                !text.contains(word),
                "{word} is in {} (the scratch world holds transcript text)",
                path.display()
            );
        }
    }
}
