// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless check of what a key chose on an approval box (session-log fix E8, the lander's
//! review): a real headless instance, its real control verbs (`key`, `send`, a `feed-bin` frame)
//! and its person seam (`key as=person`, a debug build's), on a Claude Code box a stand-in draws
//! under the name `claude`. Each key's close says what the option the screen showed it landing
//! on chose, at `conf=heuristic`, with `by=` the input's author.

#![cfg(unix)]

use std::path::Path;
use std::time::{Duration, Instant};

use crate::headless_boot;
use crate::live_ctl::Ctl;

use headless_boot::{Instance, boot_session, ctl_ok, field, process_tree, status, type_line};

const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;
const HANG_MS: &str = "timeout=60000";
/// The instance's rows: each screen is drawn at their foot, as Claude Code draws inline.
const LINES: usize = 40;

/// A Claude Code stand-in: it draws `busy.scr`, then `box.scr` each time the test writes a new
/// round into `round` (looked at each second, or at a key), and `busy.scr` again on any key it
/// reads (macOS's bash 3.2 takes whole seconds for `read -t`). Each screen is one synchronized
/// frame (DEC 2026), so no read of the screen lands mid-draw.
const STANDIN: &str = r#"#!/bin/bash
[ "$1" = --version ] && { echo 2.1.300; exit 0; }
dir="$1"
stty -icanon -echo 2>/dev/null
show() { printf '\033[?2026h\033[H\033[2J'; cat "$dir/$1.scr"; printf '\033[?2026l'; }
show busy
drawn=""
while :; do
  want=$(cat "$dir/round" 2>/dev/null)
  if [ -n "$want" ] && [ "$want" != "$drawn" ]; then drawn="$want"; show box; fi
  if IFS= read -r -s -n 1 -t 1 _; then show busy; fi
done
"#;

/// The committed Claude Code screen `fixture` (aterm-phase's), drawn at the foot of the screen.
fn screen(fixture: &str) -> String {
    let path =
        aterm_tempfile::manifest_dir().join(format!("../aterm-phase/src/fixtures/{fixture}.txt"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let body = match text.strip_prefix("# ") {
        Some(rest) => rest.split_once('\n').map_or("", |(_, b)| b),
        None => text.as_str(),
    };
    let rows: Vec<&str> = body.lines().collect();
    let mut all = vec![""; LINES.saturating_sub(rows.len())];
    all.extend(rows);
    all.join("\r\n")
}

fn cursor(inst: &Instance, sid: &str) -> (String, String) {
    let st = status(inst, sid);
    (
        field(&st, "log_seq").expect("log_seq").to_string(),
        field(&st, "log_ep").expect("log_ep").to_string(),
    )
}

/// The first record past `at` that `what` names.
fn await_log(inst: &Instance, sid: &str, what: &str, at: &(String, String)) -> String {
    let out = headless_boot::ctl(
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
    let reply = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success() && reply.starts_with("OK seq="),
        "await log {what}: {reply}\nstatus: {}\nscreen:\n{}\nlog:\n{}",
        status(inst, sid),
        ctl_ok(inst, &[&format!("@{sid}"), "text"]),
        ctl_ok(inst, &[&format!("@{sid}"), "log", "tier=host", "n=200"])
    );
    reply
}

/// The first approval step's record past `at` whose op is `op` (`open`, `close`), read on past
/// the approval records before it.
fn await_approval(inst: &Instance, sid: &str, op: &str, at: &(String, String)) -> String {
    let mut at = at.clone();
    loop {
        let reply = await_log(inst, sid, "kind=approval", &at);
        if reply.split_whitespace().any(|t| t == op) {
            return reply;
        }
        at = (
            field(&reply, "seq").expect("seq").to_string(),
            field(&reply, "ep").expect("ep").to_string(),
        );
    }
}

/// How a key reaches the box.
#[derive(Clone, Copy, Debug)]
enum Press {
    /// `@<sid> key <name>`.
    Key(&'static str),
    /// `@<sid> send <text>`.
    Send(&'static str),
    /// A `feed-bin` frame of these bytes to the front session.
    FeedBin(&'static [u8]),
    /// `@<sid> key as=person <name>`: the human input seam.
    Person(&'static str),
}

/// FIX E8 THROUGH THE REAL VERBS: on a Claude Code Bash box (`❯ 1. Yes`, `2. No`), a control
/// client's `key enter`, `send \n` and a `feed-bin` CR approve, its `key 2` declines, and a
/// person's Enter (the debug build's `key as=person`) approves, each closing the box at
/// `conf=heuristic` and `by=` the input's author. NEGATIVE CONTROLS (the lander's, each a planted
/// break that fails this test): a sweep that notes no frame closes every box `passed`; a control
/// verb whose input mark takes no key class closes its keys `passed`; a `feed-bin` frame whose
/// mark takes none closes its CR `passed`.
#[test]
fn a_key_on_a_claude_code_box_closes_it_by_the_option_it_chose() {
    let inst = headless_boot::boot_with(
        "atsko",
        |_, cmd| {
            cmd.args(["--lines", &LINES.to_string(), "--columns", "140"]);
        },
        |_| true,
    );
    let (_, sid) = boot_session(&inst);
    let dir = inst.tmp.join("screens");
    std::fs::create_dir_all(&dir).expect("the screens' dir");
    std::fs::write(dir.join("busy.scr"), screen("claude-2.1.283-footer-busy")).expect("busy.scr");
    std::fs::write(
        dir.join("box.scr"),
        screen("claude-2.1.282-box-bash-ask-rule"),
    )
    .expect("box.scr");
    let script = inst.tmp.join("claude.sh");
    std::fs::write(&script, STANDIN).expect("the stand-in");
    let bash = aterm_uds::deadline::stand_in(Path::new("/bin/bash"));
    type_line(
        &inst,
        &sid,
        &format!(
            "/bin/bash -c 'exec -a claude {} {} {}'",
            bash.display(),
            script.display(),
            dir.display()
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
    let mut front = Ctl::connect(&inst.sock);
    let rounds: [(Press, &str, &str); 5] = [
        (Press::Key("enter"), "approved", "ctl"),
        (Press::Key("2"), "declined", "ctl"),
        (Press::Send("\\n"), "approved", "ctl"),
        (Press::FeedBin(b"\r"), "approved", "ctl"),
        (Press::Person("enter"), "approved", "human"),
    ];
    let mut failures = Vec::new();
    for (round, (press, out, by)) in rounds.into_iter().enumerate() {
        let before_box = cursor(&inst, &sid);
        std::fs::write(dir.join("round"), round.to_string()).expect("the round");
        let _opened = await_approval(&inst, &sid, "open", &before_box);
        let before_key = cursor(&inst, &sid);
        let at = format!("@{sid}");
        match press {
            Press::Key(name) => {
                ctl_ok(&inst, &[&at, "key", name]);
            }
            Press::Send(text) => {
                ctl_ok(&inst, &[&at, "send", text]);
            }
            Press::FeedBin(bytes) => {
                front.send(std::str::from_utf8(bytes).expect("ascii"));
            }
            Press::Person(name) => {
                ctl_ok(&inst, &[&at, "key", "as=person", name]);
            }
        }
        let closed = await_approval(&inst, &sid, "close", &before_key);
        let want = [
            format!(" out={out} "),
            format!(" by={by} "),
            " conf=heuristic ".to_string(),
        ];
        if !want.iter().all(|w| closed.contains(w.as_str())) {
            failures.push(format!("{press:?}: {closed}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
