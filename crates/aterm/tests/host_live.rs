// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless conformance for the IN-GUI SUPERVISOR HOST (`aterm-gui`'s `harness_host`): a headless instance supervises every Claude Code session it owns by default, with nothing started by hand — no `aterm supervise`, no command typed for it.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_agent::supervise::QuestionPriority;
use aterm_agent::supervise::prompt::fixtures::{
    QUESTION_283_FREE_TEXT_FOCUSED, QUESTION_283_MULTISELECT_FREE_TEXT_FOCUSED,
    QUESTION_283_MULTISELECT_REVIEW, QUESTION_283_MULTISELECT_TYPED_BUTTON,
    QUESTION_283_MULTISELECT_TYPED_LONG_TERM, QUESTION_283_TYPED_LONG_TERM,
    QUESTION_283_TYPED_PERSON, QUESTION_283_TYPED_SHORT_TERM, rewrap_free_text, screen,
};

use crate::{headless_boot, launch_isolation};

#[cfg(target_os = "macos")]
use headless_boot::ctl;
use headless_boot::{ctl_ok, field, log_tail};

const HOLD_S: u32 = 5;
const UNTIL_MAX_TENTHS: u32 = 200;
const HANG: Duration = Duration::from_secs(60);

struct Instance {
    live: headless_boot::Instance,
    sid: String,
}

impl std::ops::Deref for Instance {
    type Target = headless_boot::Instance;
    fn deref(&self) -> &Self::Target {
        &self.live
    }
}

impl std::ops::DerefMut for Instance {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.live
    }
}

fn boot(tag: &str, harness: &str) -> Instance {
    boot_sized(tag, harness, 40, None)
}

#[cfg(target_os = "macos")]
const SHORT_SETTLE_S: u64 = 2;

#[cfg(target_os = "macos")]
fn boot_upgrade(tag: &str, harness: &str, settle_s: Option<u64>) -> Instance {
    boot_sized(tag, harness, 40, settle_s)
}

#[cfg(target_os = "macos")]
fn boot_upgrade_on_the_shipped_clock(tag: &str, harness: &str, settle_s: Option<u64>) -> Instance {
    boot_on(tag, harness, 40, settle_s, false, |_| Vec::new())
}

fn boot_sized(tag: &str, harness: &str, lines: u16, settle_s: Option<u64>) -> Instance {
    boot_on(tag, harness, lines, settle_s, true, |_| Vec::new())
}

fn boot_on(
    tag: &str,
    harness: &str,
    lines: u16,
    settle_s: Option<u64>,
    fast: bool,
    seams: impl FnOnce(&Path) -> Vec<(&'static str, String)>,
) -> Instance {
    static TAKEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    {
        let mut taken = TAKEN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            !taken.iter().any(|t| t == tag),
            "boot tag `{tag}` is taken: two boots would share one scratch root"
        );
        taken.push(tag.to_owned());
    }
    let live = headless_boot::boot_with(
        &format!("athh{tag}"),
        |tmp, cmd| {
            let cfg = tmp.join("cfg/aterm/aterm.toml");
            let base = std::fs::read_to_string(&cfg).expect("the isolation config");
            assert!(base.contains(launch_isolation::HARNESS_OFF), "{base}");
            let base = base.replace(launch_isolation::HARNESS_OFF, "");
            std::fs::write(&cfg, format!("{base}[harness]\n{harness}")).expect("write the config");
            write_world(tmp);
            cmd.env("ATERM_LOG", "info")
                .args(["--lines", &lines.to_string(), "--columns", "200"]);
            if let Some(settle) = settle_s {
                cmd.env(
                    aterm_agent::harness::upgrade::QUIET_SEAM,
                    settle.to_string(),
                );
            }
            if !fast {
                cmd.env_remove(aterm_agent::harness::timings::FAST_SEAM);
            }
            for (key, value) in seams(tmp) {
                cmd.env(key, value);
            }
        },
        |_| true,
    );
    let mut inst = Instance {
        live,
        sid: String::new(),
    };
    inst.sid = boot_session(&inst);
    inst
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(hex) = s.get(i + 1..i + 3)
            && let Ok(v) = u8::from_str_radix(hex, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn boot_session(inst: &Instance) -> String {
    let body = ctl_ok(inst, &["sessions"]);
    let row = body
        .lines()
        .find(|l| l.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or_else(|| panic!("a session row: {body}"));
    row.split_whitespace().nth(1).expect("a sid").to_string()
}

fn status(inst: &Instance) -> String {
    status_of(inst, &inst.sid)
}

fn status_of(inst: &Instance, sid: &str) -> String {
    ctl_ok(inst, &[&format!("@{sid}"), "status"])
}

fn spawn_tab(inst: &Instance) -> String {
    let body = ctl_ok(inst, &["spawn"]);
    body.split_whitespace()
        .find(|w| w.starts_with("s-"))
        .unwrap_or_else(|| panic!("spawn names its sid: {body}"))
        .to_string()
}

fn meta(inst: &Instance) -> String {
    ctl_ok(inst, &[&format!("@{}", inst.sid), "meta"])
}

fn attention(inst: &Instance) -> Option<String> {
    let m = meta(inst);
    field(&m, "attention").filter(|v| *v != "-").map(pct_decode)
}

fn harness_lines(inst: &Instance) -> String {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.file_name().is_some_and(|n| n == "aterm.log") {
                out.push(p);
            }
        }
    }
    let mut logs = Vec::new();
    walk(&inst.tmp.join("home"), &mut logs);
    walk(&inst.tmp.join("state"), &mut logs);
    logs.iter()
        .flat_map(|p| {
            std::fs::read_to_string(p)
                .unwrap_or_default()
                .lines()
                .filter(|l| l.contains("harness"))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let p = e.path();
        if p.is_dir() {
            find_file(&p, name)
        } else {
            (p.file_name().is_some_and(|n| n == name)).then_some(p)
        }
    })
}

fn until(
    within: Duration,
    what: &str,
    probe: impl Fn() -> String,
    pred: impl Fn(&str) -> bool,
) -> String {
    let deadline = Instant::now() + within;
    let mut last = String::new();
    while Instant::now() < deadline {
        last = probe();
        if pred(&last) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("{what}: not within {within:?}; last: {last}");
}

fn timeline_since(inst: &Instance, after: u64) -> Vec<(u64, String, String)> {
    let since = format!("since={after}");
    ctl_ok(inst, &[&format!("@{}", inst.sid), "timeline", &since])
        .lines()
        .filter_map(|l| {
            let (id, rest) = l.strip_prefix("event ")?.split_once(" t=")?;
            let (_, rest) = rest.split_once(" kind=")?;
            let (kind, payload) = rest.split_once(' ').unwrap_or((rest, ""));
            Some((id.parse().ok()?, kind.to_string(), payload.to_string()))
        })
        .collect()
}

fn body(fixture: &str) -> String {
    fixture
        .split_once('\n')
        .map_or(fixture, |(_, rest)| rest)
        .to_string()
}

const BOX_TOUCH: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.280-box-bash-touch.txt");
const BOX_RM: &str = include_str!("../../aterm-phase/src/fixtures/claude-2.1.280-box-rm.txt");
const SURVEY: &str = include_str!("../../aterm-phase/src/fixtures/survey-open.txt");
const CARD_LONG: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.284-survey-long-context.txt");
const CARD_PLUGIN: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.284-survey-plugin.txt");
const CARD_MEMORY: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.284-survey-memory-cited.txt");
const CARD_BUSY: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.284-survey-session-busy-workflow.txt");
const CARD_SPINNER: &str = include_str!(
    "../../aterm-phase/src/fixtures/claude-2.1.284-survey-long-context-busy-spinner.txt"
);
const DRAFTS_BUSY: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.284-drafts-card-busy-workflow.txt");
const ASK_BUSY: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.284-share-ask-busy-workflow.txt");
const WEB_SETUP_BUSY: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.284-web-setup-busy-workflow.txt");
const SPINNER: &str = include_str!("../../aterm-phase/src/fixtures/spinner-tempering.txt");
const OFFER: &str = include_str!("../../aterm-phase/src/fixtures/hand-built-offer-end-of-turn.txt");
const OWNERS_BOX: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.28x-box-rm-workflow.txt");
const WALL_SHELL: &str = include_str!(
    "../../aterm-phase/src/fixtures/claude-2.1.283-wall-weekly-goal-paused-shell-measured.txt"
);
const QUESTION: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.282-question-yes-no.txt");
const Q_TAB1: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.282-question-tabs-incident.txt");
const Q_TAB2: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.282-question-tabs-second.txt");
const Q_TAB3: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.282-question-tabs-third.txt");
const Q_TAB4: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.282-question-tabs-fourth.txt");
const Q_REVIEW: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.282-question-review.txt");
const Q_ONE: &str = include_str!("../../aterm-phase/src/fixtures/claude-2.1.282-question-one.txt");
const Q_FREE_FOCUSED: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.282-question-free-text-focused.txt");
const Q_TYPED: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.282-question-free-text-typed.txt");
const BUSY_283: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.283-footer-busy.txt");
const AFTER_TURN_283: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.283-footer-after-turn.txt");

const CAPTURE_COLS: usize = 120;
const DRAWN_ROWS: usize = 38;
const VIM_HINT: &str = " · ctrl+g to edit in Vim";
const REVIEW_ROW: usize = 119;

fn is_rule(row: &str) -> bool {
    !row.is_empty() && row.chars().all(|c| c == '─')
}

fn drawn(rows: &[String]) -> String {
    let end = rows
        .iter()
        .rposition(|r| !r.trim().is_empty())
        .map_or(0, |i| i + 1);
    rows[end.saturating_sub(DRAWN_ROWS)..end].join("\n") + "\n"
}

fn dialog_top(rows: &[String]) -> usize {
    let rules: Vec<usize> = (0..rows.len()).filter(|&i| is_rule(&rows[i])).collect();
    let [.., top, _divider] = rules[..] else {
        panic!("a select tab has its top rule and its divider")
    };
    top
}

fn typed_283(capture: &str, text: &str) -> Vec<String> {
    let rows = screen(capture);
    let mut out = rows[..dialog_top(&rows)].to_vec();
    out.extend(rewrap_free_text(capture, text, CAPTURE_COLS));
    out
}

fn focus_on_option(rows: &[String], to: &str) -> Vec<String> {
    focus_moved(&(rows.join("\n") + "\n"), to)
        .replace(VIM_HINT, "")
        .lines()
        .map(str::to_string)
        .collect()
}

fn pristine_283(rows: &[String]) -> Vec<String> {
    let divider = rows.iter().rposition(|r| is_rule(r)).expect("the divider");
    let field = rows[..divider]
        .iter()
        .rposition(|r| r.starts_with("❯ "))
        .expect("the focused free-text row");
    let n: String = rows[field]["❯ ".len()..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    assert!(!n.is_empty(), "not a numbered row: {:?}", rows[field]);
    let mut out = rows[..field].to_vec();
    out.push(format!("❯ {n}. Type something."));
    out.extend_from_slice(&rows[divider..]);
    out
}

fn review_283(capture: &str, text: &str) -> Vec<String> {
    let rows = screen(capture);
    let first = rows
        .iter()
        .position(|r| r.starts_with("   → \""))
        .expect("the review's answer row");
    let last = (first..rows.len())
        .find(|&i| rows[i].ends_with('"'))
        .expect("the answer's closing quote");
    let mut out = rows[..first].to_vec();
    let mut line = "   → ".to_string();
    let mut empty = true;
    for word in format!("\"{text}\"").split(' ') {
        if !empty && line.chars().count() + 1 + word.chars().count() > REVIEW_ROW {
            out.push(std::mem::replace(&mut line, "   ".to_string()));
            empty = true;
        }
        if !empty {
            line.push(' ');
        }
        line.push_str(word);
        empty = false;
    }
    out.push(line);
    out.extend_from_slice(&rows[last + 1..]);
    out
}

fn text_read(p: QuestionPriority) -> String {
    format!("text:{}", p.text().expect("a policy text"))
}

fn bypass_start() -> String {
    let banner: Vec<String> = body(BOX_TOUCH)
        .lines()
        .take(4)
        .map(str::to_string)
        .collect();
    let offer = body(OFFER);
    let composer: Vec<&str> = offer.lines().rev().take(4).collect();
    let mut rows = banner;
    rows.push(String::new());
    rows.extend(composer.into_iter().rev().map(str::to_string));
    rows.join("\n") + "\n"
}

fn rm_box(cmd: &str) -> String {
    body(BOX_RM)
        .replace(
            "S=$PWD/tmp; for p in a b; do set -- $p; rm -rf $S/$1; done",
            cmd,
        )
        .replace(
            "Removing directories tmp/a and tmp/b",
            "Removing the directory",
        )
        .replace("Remove directories tmp/a and tmp/b", "Remove the directory")
        .replace("$S/$1 in `rm -rf $S/$1`", "$S/a in `rm -rf $S/a`")
}

fn tall_box(lines: usize) -> String {
    let mut r = vec![
        "⏺ Writing the fixture file.".to_string(),
        String::new(),
        "─".repeat(120),
        " Bash command · from the \"fixtures\" workflow".to_string(),
        String::new(),
        "   │ cat > notes.txt <<'EOF'".to_string(),
    ];
    r.extend((1..lines).map(|i| format!("   │ line {i} of the heredoc")));
    for row in [
        "   Write the notes file",
        "",
        " Do you want to proceed?",
        " ❯ 1. Yes",
        "   2. No",
        "",
        " Esc to cancel · Tab to amend",
    ] {
        r.push(row.to_string());
    }
    r.join("\n")
}

fn focus_moved(screen: &str, to: &str) -> String {
    let from = screen
        .lines()
        .find(|r| {
            r.strip_prefix("❯ ")
                .is_some_and(|t| t.starts_with(|c: char| c.is_ascii_digit()))
        })
        .expect("a focused option row")
        .to_string();
    let target = format!("  {to}");
    assert!(screen.lines().any(|r| r == target), "no row {target:?}");
    screen
        .replace(&from, &format!("  {}", &from["❯ ".len()..]))
        .replace(&target, &format!("❯ {to}"))
}

fn from_the_rule(screen: &str) -> String {
    let rows: Vec<&str> = screen.lines().collect();
    let rule = rows
        .iter()
        .position(|r| r.starts_with('─'))
        .expect("the dialog's rule");
    rows[rule..].join("\n") + "\n"
}

fn fake_path(tmp: &Path) -> PathBuf {
    atpkg::store::default_prefix(&tmp.join("home")).join("agents/claude")
}

fn place_fake(fake: &Path, text: &str) {
    headless_boot::standin(fake, text);
}

fn write_world(tmp: &Path) {
    let claude = fake_path(tmp);
    let bin = claude.parent().expect("agents/").to_path_buf();
    let sc = tmp.join("sc");
    for d in [&bin, &sc, &tmp.join("work")] {
        std::fs::create_dir_all(d).expect("mkdir");
    }
    let fake = format!(
        "#!/bin/sh\n\
         # A fake Claude Code: draws each screen file given, reads ONE key after\n\
         # each into the key log ($1) -- one byte, or ESC and the two after it --\n\
         # as `od -c` spells it, a space as `space`; a read that returns no byte\n\
         # is no key (noted in $log.noread, then made again, as a real program's\n\
         # blocking read waits on); a *.hold screen is shown and nothing read, a\n\
         # *.until one the same until $log.release exists (or for a bound), a\n\
         # *.gate one until it exists, a *.look one until $log.next exists\n\
         # (taken: the test makes one per look), a *.late one is that wait\n\
         # with nothing drawn, and after a *.type screen ONE bounded read takes\n\
         # what was written, logged text:. A *.hold's seconds start once\n\
         # $log.go exists (the test says when: the host has attached).\n\
         # It first clears what the shell printed between its fork and this\n\
         # exec, then writes a cell and erases it in place, as a real Claude\n\
         # Code moves the screen as it starts (start_fake says why).\n\
         log=$1; shift\n\
         printf '\\033[2J\\033[H'\n\
         printf 'x\\b \\b'\n\
         old=$(stty -g)\n\
         stty -icanon -echo min 1\n\
         for scene in \"$@\"; do\n\
         \x20 case \"$scene\" in *.late) while [ ! -e \"$log.next\" ]; do sleep 0.1; done; rm -f \"$log.next\"; continue ;; esac\n\
         \x20 printf '\\033[2J\\033[H'\n\
         \x20 cat \"$scene\"\n\
         \x20 r=$(grep -n '^❯' \"$scene\" | tail -n 1 | cut -d: -f1); [ -n \"$r\" ] && printf '\\033[%s;3H' \"$r\"\n\
         \x20 case \"$scene\" in\n\
         \x20 *.hold) while [ ! -e \"$log.go\" ]; do sleep 0.1; done; sleep {HOLD_S}; continue ;;\n\
         \x20 *.until) n=0; while [ ! -e \"$log.release\" ] && [ $n -lt {UNTIL_MAX_TENTHS} ]; do sleep 0.1; n=$((n+1)); done; continue ;;\n\
         \x20 *.gate) while [ ! -e \"$log.release\" ]; do sleep 0.1; done; continue ;;\n\
         \x20 *.look) while [ ! -e \"$log.next\" ]; do sleep 0.1; done; rm -f \"$log.next\"; continue ;;\n\
         \x20 *.type) t=$(dd bs=4096 count=1 2>/dev/null); printf 'text:%s\\n' \"$t\" >> \"$log\"; continue ;;\n\
         \x20 esac\n\
         \x20 while :; do\n\
         \x20   k=$(dd bs=1 count=1 2>\"$log.dd\" | od -An -c | tr -d ' ')\n\
         \x20   grep -q '^1+0 records in' \"$log.dd\" && break\n\
         \x20   tr '\\n' ' ' < \"$log.dd\" >> \"$log.noread\"; echo >> \"$log.noread\"; sleep 0.05\n\
         \x20 done\n\
         \x20 [ -z \"$k\" ] && k=space\n\
         \x20 case \"$k\" in 033) k=\"$k$(dd bs=1 count=2 2>/dev/null | od -An -c | tr -d ' ')\" ;; esac\n\
         \x20 printf '%s\\n' \"$k\" >> \"$log\"\n\
         done\n\
         stty \"$old\"\n\
         printf '\\033[2J\\033[H'\n"
    );
    place_fake(&claude, &fake);
    let read = body(BOX_TOUCH)
        .replace("touch x", "ls -la")
        .replace("Create file x", "List the files")
        .replace("3. Nooject", "3. No");
    let survey = body(SURVEY).replace(
        "❯ Fix the parser path break yourself, same rules as the last one",
        "❯ ",
    );
    let screens = [
        ("read.txt", read),
        ("touch.txt", body(BOX_TOUCH)),
        ("question.txt", body(QUESTION)),
        ("survey.txt", survey),
        ("card-long.txt", body(CARD_LONG)),
        ("card-plugin.txt", body(CARD_PLUGIN)),
        ("card-memory.txt", body(CARD_MEMORY)),
        ("card-busy.txt", body(CARD_BUSY)),
        ("card-spinner.txt", body(CARD_SPINNER)),
        ("drafts-busy.txt", body(DRAFTS_BUSY)),
        ("ask-busy.txt", body(ASK_BUSY)),
        ("web-setup-busy.txt", body(WEB_SETUP_BUSY)),
        ("busy.hold", body(SPINNER)),
        ("busy.gate", body(SPINNER)),
        (
            "wall.hold",
            body(WALL_SHELL).replace("resets 4pm (America/Los_Angeles)", "resets in 1h"),
        ),
        ("bypass.look", bypass_start()),
        ("rm-usr.txt", rm_box("S=/usr; rm -rf $S/a")),
        ("owners-box.txt", body(OWNERS_BOX)),
        ("tall.txt", tall_box(33)),
        ("q-tab1.txt", body(Q_TAB1)),
        ("q-tab2.txt", body(Q_TAB2)),
        ("q-tab3.txt", body(Q_TAB3)),
        ("q-tab4.txt", body(Q_TAB4)),
        ("q-review.txt", body(Q_REVIEW)),
        ("q-one.txt", body(Q_ONE)),
        ("q-one-top.txt", from_the_rule(&body(Q_ONE))),
        ("q-free-focused.txt", body(Q_FREE_FOCUSED)),
        (
            "q-free-focus3.txt",
            focus_moved(&body(Q_FREE_FOCUSED), "3. High contrast"),
        ),
        (
            "q-free-focus2.txt",
            focus_moved(&body(Q_FREE_FOCUSED), "2. Light"),
        ),
        ("q-typed.txt", body(Q_TYPED)),
    ];
    for (name, text) in screens {
        std::fs::write(sc.join(name), text).expect("write a screen");
    }
    let long = QuestionPriority::LongTerm
        .text()
        .expect("the long-term text");
    let short = QuestionPriority::ShortTerm
        .text()
        .expect("the short-term text");
    let c1_field = screen(QUESTION_283_FREE_TEXT_FOCUSED);
    let c3_field = screen(QUESTION_283_MULTISELECT_FREE_TEXT_FOCUSED);
    let c8_field = pristine_283(&screen(QUESTION_283_TYPED_SHORT_TERM));
    let screens_283 = [
        (
            "c1-opt1.txt",
            focus_on_option(&c1_field, "1. Red (Recommended)"),
        ),
        ("c1-opt2.txt", focus_on_option(&c1_field, "2. Blue")),
        ("c1-field.type", c1_field.clone()),
        ("c1-long.txt", typed_283(QUESTION_283_TYPED_LONG_TERM, long)),
        (
            "c3-opt1.txt",
            focus_on_option(&c3_field, "1. [ ] Cheese (Recommended)"),
        ),
        ("c3-opt2.txt", focus_on_option(&c3_field, "2. [ ] Olives")),
        ("c3-opt3.txt", focus_on_option(&c3_field, "3. [ ] Basil")),
        ("c3-field.type", c3_field.clone()),
        (
            "c3-long.txt",
            typed_283(QUESTION_283_MULTISELECT_TYPED_LONG_TERM, long),
        ),
        (
            "c3-long-button.txt",
            typed_283(QUESTION_283_MULTISELECT_TYPED_BUTTON, long),
        ),
        (
            "c3-review.txt",
            review_283(QUESTION_283_MULTISELECT_REVIEW, long),
        ),
        (
            "c8-opt1.txt",
            focus_on_option(&c8_field, "1. Red (Recommended)"),
        ),
        ("c8-opt2.txt", focus_on_option(&c8_field, "2. Blue")),
        ("c8-field.type", c8_field.clone()),
        (
            "c8-short.txt",
            typed_283(QUESTION_283_TYPED_SHORT_TERM, short),
        ),
        ("c2-person.type", screen(QUESTION_283_TYPED_PERSON)),
        ("worked.hold", screen(BUSY_283)),
        ("worked.until", screen(BUSY_283)),
        ("done.look", screen(AFTER_TURN_283)),
    ];
    for (name, rows) in screens_283 {
        std::fs::write(sc.join(name), drawn(&rows)).expect("write a screen");
    }
}

fn run_fake(inst: &Instance, keys: &str, screens: &[&str]) {
    std::fs::write(inst.tmp.join(format!("{keys}.go")), b"").expect("start the fake's clock");
    start_fake(inst, keys, screens);
}

fn run_fake_attached(inst: &Instance, keys: &str, screens: &[&str]) {
    no_earlier_claim(inst);
    let go = inst.tmp.join(format!("{keys}.go"));
    let _ = std::fs::remove_file(&go);
    let mark = timeline_since(inst, 0).last().map_or(0, |e| e.0);
    let started = Instant::now();
    start_fake(inst, keys, screens);
    until_attached(inst, keys, mark);
    eprintln!(
        "measured, not judged: the host attached to {keys} {:?} after its start",
        started.elapsed()
    );
    std::fs::write(&go, b"").expect("start the fake's clock");
}

/// Waits for the host to attach to `keys`' fake: a `supervisor=aterm-harness` meta change on
/// the timeline after `mark`.
fn until_attached(inst: &Instance, keys: &str, mark: u64) {
    let deadline = Instant::now() + HANG;
    loop {
        let events = timeline_since(inst, mark);
        if events.iter().any(|e| {
            e.1 == "meta-change" && e.2.starts_with("field=supervisor value=aterm-harness")
        }) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the host attached to {keys}: not within {HANG:?} (a hang); timeline: {events:?}\n\
             harness log:\n{}",
            harness_lines(inst)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn no_earlier_claim(inst: &Instance) {
    until(
        HANG,
        "the host released any earlier run's claim (or its lease lapsed)",
        || status(inst),
        |s| field(s, "supervisor") == Some("-"),
    );
}

fn journal_records(inst: &Instance) -> Vec<aterm_agent::supervise::journal::JournalRecord> {
    journal(inst)
        .lines()
        .filter_map(|l| aterm_agent::supervise::journal::JournalRecord::from_json(l).ok())
        .collect()
}

fn run_fake_looked(inst: &Instance, keys: &str, screens: &[&str]) -> usize {
    no_earlier_claim(inst);
    let mut mark = journal_records(inst).len();
    let attach = timeline_since(inst, 0).last().map_or(0, |e| e.0);
    start_fake(inst, keys, screens);
    for scene in screens
        .iter()
        .take_while(|s| s.ends_with(".late") || s.ends_with(".look"))
    {
        if scene.ends_with(".late") {
            // A screen the agent has not drawn on is no point (it is still starting), so the
            // host looks at none: the agent draws once the host has attached to it.
            until_attached(inst, keys, attach);
            std::fs::write(inst.tmp.join(format!("{keys}.next")), b"").expect("let the fake go on");
        } else {
            mark = let_go_once_looked(inst, keys, mark);
        }
    }
    mark
}

fn let_go_once_looked(inst: &Instance, keys: &str, mark: usize) -> usize {
    let deadline = Instant::now() + HANG;
    loop {
        let records = journal_records(inst);
        if let Some(at) = records
            .iter()
            .skip(mark)
            .position(|r| r.kind == "event" && r.summary != "-")
        {
            std::fs::write(inst.tmp.join(format!("{keys}.next")), b"").expect("let the fake go on");
            return mark + at + 1;
        }
        assert!(
            Instant::now() < deadline,
            "the host's look at {keys}'s drawn scene: not within {HANG:?} (a hang); journal:\n{}\n\
             harness log:\n{}",
            journal(inst),
            harness_lines(inst)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn start_fake(inst: &Instance, keys: &str, screens: &[&str]) {
    start_fake_in(inst, &inst.sid, keys, screens);
}

fn start_fake_in(inst: &Instance, sid: &str, keys: &str, screens: &[&str]) {
    let tmp = inst.tmp.display();
    let scenes: Vec<String> = screens.iter().map(|s| format!("{tmp}/sc/{s}")).collect();
    let line = format!(
        "clear; cd {tmp}/work; printf '\\033]7;file://localhost%s\\007' \"$PWD\"; \
         '{}' {tmp}/{keys} {}",
        fake_path(&inst.tmp).display(),
        scenes.join(" ")
    );
    let sel = format!("@{sid}");
    ctl_ok(inst, &[&sel, "send", &line]);
    ctl_ok(inst, &[&sel, "key", "enter"]);
}

fn keys(inst: &Instance, log: &str) -> Vec<String> {
    std::fs::read_to_string(inst.tmp.join(log))
        .unwrap_or_default()
        .lines()
        .map(|k| {
            match k {
                "\\r" | "\\n" => "enter",
                "033[A" | "033OA" => "up",
                "033[B" | "033OB" => "down",
                other => other,
            }
            .to_string()
        })
        .collect()
}

fn ledger(inst: &Instance) -> String {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|d| d == "drive")
                && p.extension().is_some_and(|x| x == "jsonl")
                && !p.to_string_lossy().ends_with(".journal.jsonl")
            {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(&inst.tmp, &mut files);
    files
        .iter()
        .map(|p| std::fs::read_to_string(p).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

fn chose(inst: &Instance, within: Duration) -> String {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Some(line) = journal(inst).lines().find(|l| l.contains("CHOSE seq=")) {
            return line.to_string();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "no CHOSE within {within:?}; journal:\n{}\nharness log:\n{}\n\
         processes under the instance (pid ppid stat wchan etime command):\n{}",
        journal(inst),
        harness_lines(inst),
        headless_boot::process_tree(inst)
    );
}

fn ledger_rows(inst: &Instance, rule: &str, decision: &str) -> Vec<String> {
    let rule = format!("\"rule_id\":\"{rule}\"");
    let decision = format!("\"decision\":\"{decision}\"");
    ledger(inst)
        .lines()
        .filter(|r| r.contains(&rule) && r.contains(&decision))
        .map(str::to_string)
        .collect()
}

/// What the fake's reads that returned no byte said (its `$log.noread`): none is a key.
fn no_reads(inst: &Instance, log: &str) -> String {
    std::fs::read_to_string(inst.tmp.join(format!("{log}.noread"))).unwrap_or_default()
}

fn keys_until(inst: &Instance, log: &str, want: &[&str], within: Duration) {
    let mut deadline = Instant::now() + within;
    let mut last: Vec<String> = Vec::new();
    while Instant::now() < deadline {
        let read = keys(inst, log);
        if read == want {
            return;
        }
        if read.len() > want.len() || read.iter().zip(want).any(|(got, want)| got != want) {
            panic!(
                "the fake read {read:?}, which is not a start of {want:?}\nreads with no byte:\n{}\n\
                 harness log:\n{}",
                no_reads(inst, log),
                harness_lines(inst)
            );
        }
        if read.len() > last.len() {
            deadline = Instant::now() + within;
        }
        last = read;
        std::thread::sleep(Duration::from_millis(20));
    }
    let last = format!("{last:?} attention={:?}", attention(inst));
    panic!(
        "the fake read {want:?}: no key within {within:?} of the last; last: {last}\n\
         reads with no byte:\n{}\nharness log:\n{}",
        no_reads(inst, log),
        harness_lines(inst)
    );
}

fn program_is(inst: &Instance, want: &str, within: Duration) -> String {
    program_in_is(inst, &inst.sid, want, within)
}

fn program_in_is(inst: &Instance, sid: &str, want: &str, within: Duration) -> String {
    until(
        within,
        &format!("{sid}: program={want}"),
        || status_of(inst, sid),
        |s| field(s, "program") == Some(want),
    )
}

fn escalated_and_quiet(inst: &Instance, log: &str, needle: &str) -> String {
    escalated_after(inst, log, needle, &[], HANG)
}

fn escalated_after(
    inst: &Instance,
    log: &str,
    needle: &str,
    typed: &[&str],
    within: Duration,
) -> String {
    let escalations = |j: &str| j.matches("ESCALATED seq=").count();
    let before = escalations(&journal(inst));
    let deadline = Instant::now() + within;
    let text = loop {
        let got = keys(inst, log);
        let within_typed = got.len() <= typed.len()
            && got
                .iter()
                .map(String::as_str)
                .eq(typed[..got.len()].iter().copied());
        assert!(
            within_typed,
            "the fake read {got:?}, past {typed:?}, before an attention naming {needle:?}\n\
             harness log:\n{}",
            harness_lines(inst)
        );
        let now = attention(inst).unwrap_or_default();
        if now.contains(needle) {
            break now;
        }
        assert!(
            Instant::now() < deadline,
            "an attention naming {needle:?}: not within {within:?}; last: {now:?}; the fake read \
             {got:?}\nharness log:\n{}",
            harness_lines(inst)
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    until(
        HANG,
        "the hand-over journaled (ESCALATED)",
        || journal(inst),
        |j| escalations(j) > before,
    );
    assert_eq!(keys(inst, log), typed, "typed after the hand-over");
    assert!(
        attention(inst).is_some_and(|a| a.contains(needle)),
        "the attention stands"
    );
    text
}

#[test]
fn the_host_answers_every_box_by_default() {
    let inst = boot("d", "");
    run_fake(&inst, "k1", &["rm-usr.txt"]);
    keys_until(&inst, "k1", &["1"], HANG);
    program_is(&inst, "sh", HANG);

    run_fake(&inst, "k2", &["touch.txt"]);
    keys_until(&inst, "k2", &["1"], HANG);
    program_is(&inst, "sh", HANG);

    run_fake(&inst, "k3", &["q-one.txt"]);
    keys_until(&inst, "k3", &["enter"], HANG);
    program_is(&inst, "sh", HANG);

    run_fake(&inst, "k4", &["question.txt"]);
    escalated_and_quiet(&inst, "k4", "claude question");
}

#[test]
fn a_typo_in_the_harness_table_never_stops_the_answers() {
    std::thread::scope(|scope| {
        let typo = scope.spawn(|| {
            let noisy = "approve = \"maybe\"\nenabled = \"sometimes\"\nrm_breakr = false\n\
                         some_future_key = true\n";
            let inst = boot("t", noisy);
            run_fake(&inst, "k1", &["rm-usr.txt"]);
            keys_until(&inst, "k1", &["1"], HANG);
            program_is(&inst, "sh", HANG);
        });
        let off = scope.spawn(|| {
            let inst = boot("u", "approve = \"no\"\n");
            run_fake(&inst, "k1", &["rm-usr.txt"]);
            escalated_and_quiet(&inst, "k1", "S=/usr; rm -rf $S/a");
        });
        typo.join().expect("the typo's run");
        off.join().expect("the explicit off's run");
    });
}

#[test]
fn a_49_row_screen_is_read_whole_and_the_instance_lives() {
    let mut inst = boot_sized("w", "", 49, None);
    run_fake(&inst, "k0", &["q-one-top.txt"]);
    keys_until(&inst, "k0", &["enter"], HANG);
    program_is(&inst, "sh", HANG);

    run_fake(&inst, "k1", &["question.txt"]);
    escalated_and_quiet(&inst, "k1", "claude question");
    assert!(
        matches!(inst.child.try_wait(), Ok(None)),
        "the instance died; log tail:\n{}",
        log_tail(&inst.log)
    );
    ctl_ok(&inst, &[&format!("@{}", inst.sid), "key", "z"]);
    keys_until(&inst, "k1", &["z"], HANG);
    program_is(&inst, "sh", HANG);

    run_fake(&inst, "k2", &["tall.txt"]);
    keys_until(&inst, "k2", &["1"], HANG);
}

#[test]
fn the_host_answers_a_tabbed_question_with_its_recommended_options() {
    let inst = boot("q", "");
    run_fake(
        &inst,
        "k1",
        &[
            "q-tab1.txt",
            "q-tab2.txt",
            "q-tab3.txt",
            "q-tab4.txt",
            "q-review.txt",
        ],
    );
    keys_until(
        &inst,
        "k1",
        &["enter", "enter", "enter", "enter", "enter"],
        HANG,
    );
    program_is(&inst, "sh", HANG);
    assert_eq!(attention(&inst), None, "{}", harness_lines(&inst));
    let rows = ledger(&inst);
    let answered = rows
        .lines()
        .filter(|r| {
            r.contains("\"rule_id\":\"answer-recommended@v1\"")
                && r.contains("\"decision\":\"approved\"")
        })
        .count();
    assert_eq!(
        answered,
        5,
        "{rows}\nharness log:\n{}",
        harness_lines(&inst)
    );
}

#[test]
fn the_host_moves_off_the_free_text_row_before_answering() {
    let inst = boot("f", "");
    run_fake(
        &inst,
        "k1",
        &[
            "q-free-focused.txt",
            "q-free-focus3.txt",
            "q-free-focus2.txt",
            "q-one.txt",
        ],
    );
    keys_until(&inst, "k1", &["up", "up", "up", "enter"], HANG);
}

#[test]
fn the_host_hands_over_a_question_a_person_typed_into() {
    let inst = boot("p", "");
    run_fake(&inst, "k1", &["q-typed.txt"]);
    escalated_and_quiet(&inst, "k1", "claude question");
}

#[test]
fn answer_questions_false_hands_every_question_over() {
    let inst = boot("x", "answer_questions = false\n");
    run_fake(&inst, "k1", &["q-tab1.txt"]);
    escalated_and_quiet(&inst, "k1", "claude question");
}

const LONG_TERM_ON: &str = "question_priority = \"long-term\"\n";

fn release_fake(inst: &Instance, log: &str) {
    std::fs::write(inst.tmp.join(format!("{log}.release")), b"").expect("release the fake");
}

fn told_by_the_worker(inst: &Instance, rule: &str, word: &str, approvals: usize) {
    let line = chose(inst, HANG);
    let told = journal_records(inst)
        .iter()
        .position(|r| r.kind == "chose")
        .expect("the CHOSE record just read");
    release_fake(inst, "k1");
    let_go_once_looked(inst, "k1", told + 1);
    assert!(
        line.contains(&format!("rule={rule} policy={word} ")),
        "{line}"
    );
    program_is(inst, "sh", HANG);
    assert_eq!(attention(inst), None, "{}", harness_lines(inst));
    let approved = ledger_rows(inst, rule, "approved");
    assert_eq!(
        approved.len(),
        approvals,
        "{approved:#?}\nledger:\n{}\nharness log:\n{}",
        ledger(inst),
        harness_lines(inst)
    );
}

#[test]
fn under_long_term_the_worker_decides_a_single_select_question() {
    let inst = boot("l", LONG_TERM_ON);
    run_fake(
        &inst,
        "k1",
        &[
            "c1-opt1.txt",
            "c1-opt2.txt",
            "c1-field.type",
            "c1-long.txt",
            "worked.until",
            "done.look",
        ],
    );
    let text = text_read(QuestionPriority::LongTerm);
    keys_until(&inst, "k1", &["down", "down", &text, "enter"], HANG);
    told_by_the_worker(&inst, "answer-long-term@v1", "long-term", 2);
}

#[test]
fn under_long_term_the_worker_decides_a_multi_select_question() {
    let inst = boot("m", LONG_TERM_ON);
    run_fake(
        &inst,
        "k1",
        &[
            "c3-opt1.txt",
            "c3-opt2.txt",
            "c3-opt3.txt",
            "c3-field.type",
            "c3-long.txt",
            "c3-long-button.txt",
            "c3-review.txt",
            "worked.until",
            "done.look",
        ],
    );
    let text = text_read(QuestionPriority::LongTerm);
    keys_until(
        &inst,
        "k1",
        &["down", "down", "down", &text, "down", "enter", "enter"],
        HANG,
    );
    told_by_the_worker(&inst, "answer-long-term@v1", "long-term", 3);
}

#[test]
fn a_session_s_short_term_word_decides_over_the_global_recommended() {
    let inst = boot("s", "");
    ctl_ok(
        &inst,
        &[
            &format!("@{}", inst.sid),
            "meta",
            "set",
            "questions",
            "short-term",
        ],
    );
    run_fake(
        &inst,
        "k1",
        &[
            "c8-opt1.txt",
            "c8-opt2.txt",
            "c8-field.type",
            "c8-short.txt",
            "worked.until",
            "done.look",
        ],
    );
    let text = text_read(QuestionPriority::ShortTerm);
    keys_until(&inst, "k1", &["down", "down", &text, "enter"], HANG);
    told_by_the_worker(&inst, "answer-short-term@v1", "short-term", 2);
}

#[test]
fn under_long_term_a_person_s_text_is_handed_to_them_and_nothing_is_typed() {
    let inst = boot("h", LONG_TERM_ON);
    run_fake(&inst, "k1", &["c2-person.type"]);
    escalated_and_quiet(&inst, "k1", "claude question");
}

#[test]
fn a_policy_text_the_dialog_does_not_take_is_written_once_and_handed_over() {
    let inst = boot("c", LONG_TERM_ON);
    run_fake(
        &inst,
        "k1",
        &[
            "c1-opt1.txt",
            "c1-opt2.txt",
            "c1-field.type",
            "c1-field.type",
        ],
    );
    let text = text_read(QuestionPriority::LongTerm);
    let said = escalated_after(&inst, "k1", "policy text", &["down", "down", &text], HANG);
    assert!(said.contains("claude question"), "{said}");
    assert_eq!(
        ledger_rows(&inst, "answer-long-term@v1", "approved"),
        Vec::<String>::new(),
        "no approval for a text never read back"
    );
    assert_eq!(
        ledger_rows(&inst, "answer-long-term@v1", "escalated").len(),
        1,
        "{}",
        ledger(&inst)
    );
}

#[test]
fn the_host_answers_what_the_policy_proves_safe() {
    let inst = boot("a", "");
    let mark = timeline_since(&inst, 0).last().map_or(0, |e| e.0);
    run_fake(&inst, "k1", &["read.txt"]);
    keys_until(&inst, "k1", &["1"], HANG);
    program_is(&inst, "sh", HANG);
    let events = timeline_since(&inst, mark);
    assert!(
        events.iter().any(|e| e.1 == "agent-change"),
        "never published as an agent: {events:?}"
    );
    assert!(
        events.iter().any(
            |e| e.1 == "meta-change" && e.2.starts_with("field=supervisor value=aterm-harness")
        ),
        "the host never claimed the session: {events:?}\n{}",
        harness_lines(&inst)
    );

    run_fake_attached(&inst, "k2", &["busy.hold", "survey.txt"]);
    keys_until(&inst, "k2", &["0"], HANG);
    program_is(&inst, "sh", HANG);

    until(
        HANG,
        "supervisor=- after the agent left",
        || status(&inst),
        |s| field(s, "supervisor") == Some("-"),
    );
    let log = until(
        HANG,
        "the exit journaled as the launch's own end",
        || harness_lines(&inst),
        |log| log.contains("exited; not relaunched"),
    );
    for step in log.lines().filter_map(|l| l.split_once("relaunch pid=")) {
        assert!(
            step.1.contains("step=ended:") || step.1.contains("step=busy:another-sweep"),
            "{}",
            step.1
        );
    }
    assert_eq!(field(&status(&inst), "program"), Some("sh"));
    assert_eq!(attention(&inst), None, "nothing said");
}

#[test]
fn the_host_dismisses_every_kind_of_rating_card_and_reads_a_busy_worker_busy_under_any_card() {
    let inst = boot("z", "");
    let cards = [
        ("k1", "card-long.txt"),
        ("k2", "card-plugin.txt"),
        ("k3", "card-memory.txt"),
        ("k4", "card-busy.txt"),
    ];
    let busy = [
        ("k5", "drafts-busy.txt", "the drafts card"),
        ("k6", "ask-busy.txt", "the transcript-share ask"),
        ("k7", "web-setup-busy.txt", "the web-setup offer"),
    ];
    let tabs: Vec<String> = std::iter::once(inst.sid.clone())
        .chain((1..cards.len() + busy.len()).map(|_| spawn_tab(&inst)))
        .collect();
    let (card_tabs, busy_tabs) = tabs.split_at(cards.len());
    let inst = &inst;
    std::thread::scope(|scope| {
        let mut runs = Vec::new();
        for ((log, screen), sid) in cards.into_iter().zip(card_tabs) {
            runs.push(scope.spawn(move || {
                start_fake_in(inst, sid, log, &[screen]);
                keys_until(inst, log, &["0"], HANG);
                program_in_is(inst, sid, "sh", HANG);
            }));
        }
        for ((log, screen, what), sid) in busy.into_iter().zip(busy_tabs) {
            runs.push(scope.spawn(move || {
                start_fake_in(inst, sid, log, &[screen]);
                until(
                    HANG,
                    &format!("agent=busy under {what}"),
                    || status_of(inst, sid),
                    |s| field(s, "agent") == Some("busy"),
                );
                ctl_ok(inst, &[&format!("@{sid}"), "key", "z"]);
                keys_until(inst, log, &["z"], HANG);
                program_in_is(inst, sid, "sh", HANG);
            }));
        }
        for run in runs {
            run.join()
                .unwrap_or_else(|why| std::panic::resume_unwind(why));
        }
    });
}

#[test]
fn the_host_hands_over_what_it_cannot_prove_and_types_nothing() {
    let inst = boot("e", "approve = \"safe\"\nrm_breaker_ask = true\n");
    run_fake_looked(&inst, "k1", &["start.late", "bypass.look", "rm-usr.txt"]);
    let text = escalated_and_quiet(&inst, "k1", "S=/usr; rm -rf $S/a");
    assert!(
        text.contains("rm-breaker") && text.contains("scratch root"),
        "{text}"
    );
    ctl_ok(&inst, &[&format!("@{}", inst.sid), "key", "z"]);
    keys_until(&inst, "k1", &["z"], HANG);
    program_is(&inst, "sh", HANG);
    until(
        HANG,
        "the badge cleared",
        || attention(&inst).unwrap_or_default(),
        str::is_empty,
    );

    run_fake(&inst, "k2", &["touch.txt"]);
    escalated_and_quiet(&inst, "k2", "touch x");
    let cfg = inst.tmp.join("cfg/aterm/aterm.toml");
    let text = std::fs::read_to_string(&cfg).expect("config");
    std::fs::write(&cfg, format!("{text}[harness]\nenabled = false\n"))
        .expect("rewrite the config");
    until(
        HANG,
        "supervisor=- and no badge after enabled=false",
        || format!("{} attention={:?}", status(&inst), attention(&inst)),
        |s| field(s, "supervisor") == Some("-") && s.ends_with("attention=None"),
    );
    assert!(
        keys(&inst, "k2").is_empty(),
        "typed: {:?}",
        keys(&inst, "k2")
    );
}

#[test]
fn a_box_a_person_answered_loses_its_badge_while_the_worker_works() {
    let inst = boot("b", "approve = \"none\"\n");
    run_fake(&inst, "k1", &["owners-box.txt", "busy.gate"]);
    escalated_and_quiet(&inst, "k1", "aterm-auto-target-x.noindex");
    let mark = timeline_since(&inst, 0).last().map_or(0, |e| e.0);
    ctl_ok(&inst, &[&format!("@{}", inst.sid), "key", "1"]);
    keys_until(&inst, "k1", &["1"], HANG);
    until(
        HANG,
        "the badge cleared",
        || attention(&inst).unwrap_or_default(),
        str::is_empty,
    );
    let busy_at = |events: &[(u64, String, String)]| {
        events
            .iter()
            .position(|e| e.1 == "agent-change" && e.2.starts_with("busy "))
    };
    let deadline = Instant::now() + HANG;
    while busy_at(&timeline_since(&inst, mark)).is_none() {
        assert!(
            Instant::now() < deadline,
            "the answer never set the worker working: not within {HANG:?} (a hang): {:?}",
            timeline_since(&inst, mark)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    release_fake(&inst, "k1");
    let rested_after = |events: &[(u64, String, String)]| {
        events[busy_at(events)?..]
            .iter()
            .find(|e| e.1 == "agent-change" && !e.2.starts_with("busy "))
            .cloned()
    };
    let deadline = Instant::now() + HANG;
    let (events, rested) = loop {
        let events = timeline_since(&inst, mark);
        if let Some(rested) = rested_after(&events) {
            break (events, rested);
        }
        assert!(
            Instant::now() < deadline,
            "the worker never rested after its release: not within {HANG:?} (a hang): \
             {events:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let cleared = events
        .iter()
        .find(|e| e.1 == "meta-change" && e.2 == "field=attention value=-")
        .unwrap_or_else(|| panic!("the badge's clear is not on the timeline: {events:?}"));
    assert!(
        cleared.0 < rested.0,
        "the badge cleared only when the worker stopped ({rested:?}): {events:?}"
    );
    let journal = ["state", "home"]
        .iter()
        .find_map(|root| find_file(&inst.tmp.join(root), &format!("{}.journal.jsonl", inst.sid)))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    assert!(
        journal.contains("CLEARED") && journal.contains("box attention=OK"),
        "journaled: {journal}"
    );
}

const LEASE_MS: u64 = 3_000;

#[test]
fn one_supervisor_per_session() {
    let inst = boot("o", "");
    let sel = format!("@{}", inst.sid);
    ctl_ok(
        &inst,
        &[
            &sel,
            "meta",
            "set",
            "supervisor",
            "someone-else",
            "ttl=120000",
        ],
    );
    run_fake(&inst, "k1", &["read.txt", "worked.hold"]);
    program_is(&inst, "claude", HANG);
    let claimed_ms = now_ms();
    ctl_ok(
        &inst,
        &[
            &sel,
            "meta",
            "set",
            "supervisor",
            "someone-else",
            &format!("ttl={LEASE_MS}"),
        ],
    );
    let s = status(&inst);
    assert_eq!(field(&s, "supervisor"), Some("someone-else"), "{s}");
    keys_until(&inst, "k1", &["1"], HANG);
    let approved_ms = journaled_at(&inst, "APPROVED");
    assert!(
        approved_ms >= claimed_ms + u128::from(LEASE_MS),
        "answered at {approved_ms} ms, under another's lease taken at {claimed_ms} ms for \
         {LEASE_MS} ms"
    );
    let s = status(&inst);
    assert!(
        field(&s, "supervisor").is_some_and(|v| v.starts_with("aterm-harness")),
        "{s}"
    );
}

#[test]
fn autopilot_off_keeps_the_hosts_hands_off_until_the_session_opts_in() {
    let inst = boot("ap", "autopilot = \"off\"\n");
    run_fake(&inst, "k1", &["read.txt"]);
    program_is(&inst, "claude", HANG);
    assert_eq!(field(&meta(&inst), "autopilot"), Some("-"));
    let s = status(&inst);
    assert_eq!(field(&s, "supervisor"), Some("-"), "{s}");
    assert_eq!(field(&s, "autopilot"), Some("off"), "{s}");
    ctl_ok(
        &inst,
        &[
            &format!("@{}", inst.sid),
            "meta",
            "set",
            "autopilot",
            "cruise",
        ],
    );
    keys_until(&inst, "k1", &["1"], HANG);
    assert_eq!(field(&meta(&inst), "autopilot"), Some("cruise"));
}

const CONVERSATION: &str = "0badf00d-1111-2222-3333-444455556666";

fn write_relaunch_fake(inst: &Instance) {
    std::fs::write(inst.tmp.join("sc/idle.txt"), bypass_start()).expect("idle screen");
    let fake = format!(
        "#!/bin/sh\n\
         case \"$1\" in --version) echo '2.1.281 (Claude Code)'; exit 0 ;; esac\n\
         tmp=${{HOME%/home}}\n\
         echo \"$$ $*\" >> \"$tmp/launches.log\"\n\
         stty -icanon -echo min 0 time 2 2>/dev/null\n\
         sid={CONVERSATION}; prev=\n\
         for a in \"$@\"; do [ \"$prev\" = --resume ] && sid=$a; prev=$a; done\n\
         start=$(LC_ALL=C TZ=UTC ps -o lstart= -p $$ | awk '{{$1=$1; print}}')\n\
         mkdir -p \"$HOME/.claude/sessions\" \"$HOME/.claude/projects/-w\"\n\
         printf '{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"hi\"}}}}\\n' \
         >> \"$HOME/.claude/projects/-w/$sid.jsonl\"\n\
         printf '{{\"pid\":%s,\"sessionId\":\"%s\",\"cwd\":\"%s\",\"version\":\"2.1.281\",\
         \"status\":\"idle\",\"statusUpdatedAt\":%s000,\"procStart\":\"%s\",\
         \"kind\":\"interactive\",\"entrypoint\":\"cli\"}}' \
         $$ \"$sid\" \"$PWD\" \"$(date +%s)\" \"$start\" > \"$HOME/.claude/sessions/$$.json\"\n\
         printf '\\033[?1049h\\033[?1004h\\033[2J\\033[H'\n\
         cat \"$tmp/sc/idle.txt\"\n\
         r=$(grep -n '^❯' \"$tmp/sc/idle.txt\" | tail -n 1 | cut -d: -f1); [ -n \"$r\" ] && printf '\\033[%s;3H' \"$r\"\n\
         cr=$(printf '\\r'); nl=$(printf '\\n.'); nl=${{nl%.}}; esc=$(printf '\\033')\n\
         while :; do\n\
         \x20 if [ -e \"$tmp/die\" ]; then rm -f \"$tmp/die\"; kill -KILL $$; fi\n\
         \x20 got=$(dd bs=4096 count=1 2>/dev/null; echo .); got=${{got%.}}\n\
         \x20 [ -n \"$got\" ] && printf '%s' \"$got\" >> \"$tmp/typed-$$.log\"\n\
         \x20 case \"$got\" in\n\
         \x20 '') ;;\n\
         \x20 *\"$cr\"*|*\"$nl\"*) printf '\\033[2J\\033[H'; cat \"$tmp/sc/idle.txt\"; printf '\\033[%s;3H' \"$r\" ;;\n\
         \x20 \"$esc\"*) ;;\n\
         \x20 *) printf '\\033[%s;3H\\033[K[Pasted text #1]' \"$r\" ;;\n\
         \x20 esac\n\
         done\n"
    );
    place_fake(&fake_path(&inst.tmp), &fake);
}

fn launches(inst: &Instance) -> Vec<(u32, String)> {
    exec_log(inst, "launches.log")
}

#[cfg(target_os = "macos")]
fn probes(inst: &Instance) -> Vec<(u32, String)> {
    exec_log(inst, "probes.log")
}

#[cfg(target_os = "macos")]
fn probe_args(inst: &Instance) -> Vec<String> {
    probes(inst).into_iter().map(|(_, args)| args).collect()
}

/// The verdicts of the upgrade ledger's `build-probe:<verdict>` rows (`upgrade/ledger.jsonl`), in
/// order.
#[cfg(target_os = "macos")]
fn build_probes(inst: &Instance) -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.file_name().is_some_and(|n| n == "ledger.jsonl")
                && p.parent().is_some_and(|d| d.ends_with("upgrade"))
            {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(&inst.tmp, &mut files);
    files
        .iter()
        .flat_map(|p| {
            std::fs::read_to_string(p)
                .unwrap_or_default()
                .lines()
                .filter_map(|l| {
                    let (_, rest) = l.split_once("\"step\":\"build-probe:")?;
                    Some(rest.split('"').next()?.to_string())
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The upgrade's own first move probes the new build once (`<exe> --help`), its verdict on the
/// upgrade ledger. A probe that passed is the new build's own log line. One out of its time
/// (`build-probe:timed-out`, on a host too loaded to start the fake within the probe's bound) was
/// killed before the fake could log it, and the move goes ahead all the same: the ledger row is
/// then the probe's only trace.
#[cfg(target_os = "macos")]
fn probed_once(inst: &Instance) {
    let verdicts = build_probes(inst);
    assert_eq!(
        verdicts.len(),
        1,
        "the upgrade's own first move probes the new build once: {verdicts:?}"
    );
    let logged = probe_args(inst);
    if verdicts[0] == "timed-out" {
        assert!(
            logged.iter().all(|a| a == "2.1.282 --help") && logged.len() <= 1,
            "a probe out of its time logs once at most: {logged:?}"
        );
    } else {
        assert_eq!(
            logged,
            ["2.1.282 --help"],
            "the probe the new build ran ({verdicts:?})"
        );
    }
}

fn exec_log(inst: &Instance, name: &str) -> Vec<(u32, String)> {
    std::fs::read_to_string(inst.tmp.join(name))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (pid, args) = l.split_once(' ').unwrap_or((l, ""));
            Some((pid.parse().ok()?, args.to_string()))
        })
        .collect()
}

fn first_launch(inst: &Instance) -> Vec<(u32, String)> {
    let deadline = Instant::now() + HANG;
    loop {
        let got = launches(inst);
        if !got.is_empty() || Instant::now() >= deadline {
            return got;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn an_agent_that_exits_unasked_is_relaunched_on_its_conversation() {
    let inst = boot("r", "");
    write_relaunch_fake(&inst);
    let sel = format!("@{}", inst.sid);
    let tmp = inst.tmp.display();
    ctl_ok(&inst, &[&sel, "send", "/bin/bash --norc --noprofile -i"]);
    ctl_ok(&inst, &[&sel, "key", "enter"]);
    program_is(&inst, "bash", HANG);
    let line = format!(
        "cd {tmp}/work && '{}' --model opus",
        fake_path(&inst.tmp).display()
    );
    ctl_ok(&inst, &[&sel, "send", &line]);
    ctl_ok(&inst, &[&sel, "key", "enter"]);
    program_is(&inst, "claude", HANG);
    until(
        HANG,
        "the host attached",
        || status(&inst),
        |s| field(s, "supervisor").is_some_and(|v| v.starts_with("aterm-harness")),
    );
    let first = first_launch(&inst);
    assert_eq!(
        first.iter().map(|(_, a)| a.as_str()).collect::<Vec<_>>(),
        ["--model opus"],
        "{first:?}"
    );
    until(
        HANG,
        "the worker at the agent's first idle point",
        || harness_lines(&inst),
        |l| l.contains("EVENT idle"),
    );
    std::fs::write(inst.tmp.join("die"), "").expect("crash");
    let deadline = Instant::now() + HANG;
    let got = loop {
        let got = launches(&inst);
        if got.len() >= 2 || Instant::now() >= deadline {
            break got;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(
        got.len(),
        2,
        "relaunched once: {got:?}\nharness log:\n{}",
        harness_lines(&inst)
    );
    let (old, _) = &got[0];
    let (new, args) = &got[1];
    assert_ne!(old, new, "a new process");
    assert_eq!(
        args,
        &format!("--model opus --resume {CONVERSATION}"),
        "its flags kept, its conversation resumed"
    );
    program_is(&inst, "claude", HANG);
    // The continuation stays owed until a supervisor's loop over the relaunched process first
    // reaches an idle point (`IdleHost::at_idle`), and is typed there only: the record `done`
    // naming the relaunched process, and what that process read, below, are the proof.
    // `status supervisor=` names the window's host (`aterm-harness@<instance pid>`), the same
    // before the crash as after, so a wait on it tells nothing; which step the relaunch ends at
    // (`adopted` or `wait:resume`) is no part of this test.
    let name = format!("{CONVERSATION}.json");
    let record = until(
        HANG,
        "the continuation typed where the loop parked",
        || {
            ["state", "home"]
                .iter()
                .filter_map(|root| find_file(&inst.tmp.join(root), &name))
                .find(|p| p.parent().is_some_and(|d| d.ends_with("upgrade")))
                .and_then(|p| std::fs::read_to_string(p).ok())
                .unwrap_or_default()
        },
        |r| r.contains(r#""phase":"done""#) && r.contains(&format!(r#""resumed_pid":{new}"#)),
    );
    assert!(record.contains(r#""cause":"exit""#), "{record}");
    // The record says `done` once the continuation and its Enter are written; the relaunched
    // fake logs what it read after it reads it, a second or more later on a loaded host, so the
    // log is read once its Enter is in it.
    let read = until(
        HANG,
        "the relaunched agent's read of the continuation and its Enter",
        || {
            std::fs::read_to_string(inst.tmp.join(format!("typed-{new}.log")))
                .unwrap_or_default()
                .replace("\x1b[I", "")
                .replace("\x1b[O", "")
        },
        |r| r.ends_with(['\r', '\n']),
    );
    let want = aterm_agent::harness::relaunch::resumed_prompt("2.1.281", "exit");
    assert_eq!(
        read.strip_suffix(['\r', '\n']),
        Some(want.as_str()),
        "the continuation typed into the relaunched agent; record: {record}\nharness log:\n{}",
        harness_lines(&inst)
    );
}

#[cfg(target_os = "macos")]
fn stage_upgrade_fake(inst: &Instance, version: &str, asked: bool) -> PathBuf {
    assert!(
        launches(inst).is_empty(),
        "{version} staged after a launch: {:?}",
        launches(inst)
    );
    let idle = bypass_start();
    let caret = idle
        .lines()
        .enumerate()
        .filter(|(_, l)| l.starts_with('❯'))
        .last()
        .map(|(i, _)| i + 1)
        .expect("the composer's caret row");
    std::fs::write(inst.tmp.join("sc/idle.txt"), &idle).expect("idle screen");
    std::fs::write(
        inst.tmp.join("sc/idle.frame"),
        format!("\x1b[2J\x1b[H{idle}\x1b[{caret};3H"),
    )
    .expect("idle frame");
    let row = |text: &str| {
        format!(
            "printf '{{\"type\":\"assistant\",\"message\":{{\"model\":\"claude-test\",\
             \"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"%s\"}}]}},\
             \"version\":\"%s\"}}\\n' {text} \"$VERSION\" >> \"$T\""
        )
    };
    let fake = format!(
        "#!/bin/sh\n\
         VERSION={version}\n\
         tmp=${{HOME%/home}}\n\
         case \"$1\" in\n\
         --version) echo \"$VERSION (Claude Code)\"; exit 0 ;;\n\
         --help) echo \"$$ $VERSION $*\" >> \"$tmp/probes.log\"; echo 'Usage: claude [options] [command] [prompt]'; exit 0 ;;\n\
         esac\n\
         echo \"$$ $VERSION $*\" >> \"$tmp/launches.log\"\n\
         sid={CONVERSATION}; prev=\n\
         for a in \"$@\"; do [ \"$prev\" = --resume ] && sid=$a; prev=$a; done\n\
         start=$(LC_ALL=C TZ=UTC ps -o lstart= -p $$ | awk '{{$1=$1; print}}')\n\
         mkdir -p \"$HOME/.claude/sessions\" \"$HOME/.claude/projects/-w\"\n\
         T=\"$HOME/.claude/projects/-w/$sid.jsonl\"\n\
         {ask}\n\
         printf '{{\"pid\":%s,\"sessionId\":\"%s\",\"cwd\":\"%s\",\"version\":\"%s\",\
         \"status\":\"idle\",\"statusUpdatedAt\":%s000,\"procStart\":\"%s\",\
         \"kind\":\"interactive\",\"entrypoint\":\"cli\"}}' \
         $$ \"$sid\" \"$PWD\" \"$VERSION\" \"$(( $(date +%s) - 60 ))\" \"$start\" \
         > \"$HOME/.claude/sessions/$$.json\"\n\
         trap 'rm -f \"$HOME/.claude/sessions/$$.json\"; exit 0' TERM\n\
         stty -echo -icanon min 1 time 0 2>/dev/null\n\
         [ -t 0 ] && while [ -e \"$tmp/hold-idle-$VERSION\" ]; do sleep 0.05; done\n\
         W=$(stty size 2>/dev/null | cut -d' ' -f2); [ -n \"$W\" ] || W=200\n\
         W=$((W - 1))\n\
         r=$(grep -n '^❯' \"$tmp/sc/idle.txt\" | tail -n 1 | cut -d: -f1)\n\
         rule=$(sed -n \"$((r + 1))p\" \"$tmp/sc/idle.txt\"); foot=$(sed -n \"$((r + 2))p\" \"$tmp/sc/idle.txt\")\n\
         draw() {{\n\
         \x20 cat \"$tmp/sc/idle.frame\"\n\
         \x20 buf=; word=; col=2; row=$r\n\
         }}\n\
         wrap() {{\n\
         \x20 row=$((row + 1)); col=2\n\
         \x20 printf '\\033[%s;1H\\033[K  \\033[%s;1H\\033[K%s\\033[%s;1H\\033[K%s\\033[%s;3H' \
         \"$row\" $((row + 1)) \"$rule\" $((row + 2)) \"$foot\" \"$row\"\n\
         }}\n\
         take() {{\n\
         \x20 line=$1\n\
         \x20 printf '%s %s\\n' $$ \"$line\" >> \"$tmp/typed.log\"\n\
         \x20 m=$(printf '%s' \"$line\" | grep -o 'ATERM-UPGRADE-READY-[0-9a-f]*' | head -1)\n\
         \x20 [ -n \"$m\" ] && {ready}\n\
         \x20 case \"$line\" in *Upgraded*) {carry} ;; \"[aterm harness]\"*) ;; *) {prompt} ;; esac\n\
         }}\n\
         cr=$(printf '\\r'); nl=$(printf '\\n.'); nl=${{nl%.}}\n\
         draw\n\
         while IFS= read -r -n 1 -d '' c; do\n\
         \x20 case \"$c\" in\n\
         \x20 \"$cr\"|\"$nl\") take \"$buf\"; draw ;;\n\
         \x20 ' ') buf=\"$buf \"; word=\n\
         \x20   if [ \"$col\" -ge \"$W\" ]; then wrap; else printf ' '; col=$((col + 1)); fi ;;\n\
         \x20 *) buf=\"$buf$c\"\n\
         \x20   if [ $((col + 1)) -le \"$W\" ]; then printf '%s' \"$c\"; col=$((col + 1)); word=\"$word$c\"\n\
         \x20   elif [ -n \"$word\" ] && [ ${{#word}} -lt $((col - 2)) ]; then\n\
         \x20     printf '\\033[%sD\\033[K' ${{#word}}; wrap; printf '%s%s' \"$word\" \"$c\"; word=\"$word$c\"; col=$((2 + ${{#word}}))\n\
         \x20   else wrap; printf '%s' \"$c\"; word=\"$c\"; col=3; fi ;;\n\
         \x20 esac\n\
         done\n",
        ready = row("\"$m\""),
        carry = row("'carrying on'"),
        prompt = "printf '{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"%s\"}}\\n' \"$line\" >> \"$T\"",
        ask = if asked {
            format!(
                "[ -e \"$T\" ] || {{ printf '{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\
                 \"content\":\"Fix the parser.\"}}}}\\n' > \"$T\"; {}; }}",
                row("'On it.'")
            )
        } else {
            ":".to_string()
        },
    );
    let staged = fake_path(&inst.tmp).with_file_name(format!("claude.{version}"));
    place_fake(&staged, &fake);
    staged
}

#[cfg(target_os = "macos")]
fn install(inst: &Instance, staged: &Path) {
    std::fs::rename(staged, fake_path(&inst.tmp)).expect("replace the twin");
}

#[cfg(target_os = "macos")]
fn land(inst: &Instance, staged: &Path) {
    install(inst, staged);
    let prefix = atpkg::store::default_prefix(&inst.tmp.join("home"));
    let notices = prefix.join(atpkg::activation_notice::NOTICE_DIR);
    std::fs::create_dir_all(&notices).expect("notice dir");
    std::fs::write(notices.join(".m.tmp"), "1\n").expect("marker");
    std::fs::rename(
        notices.join(".m.tmp"),
        notices.join(atpkg::activation_notice::CLAUDE_MARKER),
    )
    .expect("the activation notice");
}

#[cfg(target_os = "macos")]
const UPGRADE_WITHIN: Duration = Duration::from_secs(150);

#[cfg(target_os = "macos")]
const UPGRADE_CEILING: Duration = Duration::from_secs(10 * 60);

#[cfg(target_os = "macos")]
fn version_waits(inst: &Instance) -> usize {
    journal(inst).matches("step=wait:no-version").count()
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

#[track_caller]
fn journaled_at(inst: &Instance, needle: &str) -> u128 {
    let mut last = String::new();
    let at = aterm_spec::wait::within(HANG, Duration::from_millis(20), || {
        last = journal(inst);
        last.lines().find(|l| l.contains(needle)).and_then(|l| {
            l.strip_prefix("{\"t\":")?
                .split(',')
                .next()?
                .parse::<u128>()
                .ok()
        })
    });
    at.unwrap_or_else(|| panic!("no journal record holds {needle:?} within {HANG:?}:\n{last}"))
}

fn journal(inst: &Instance) -> String {
    ["state", "home"]
        .iter()
        .find_map(|root| find_file(&inst.tmp.join(root), &format!("{}.journal.jsonl", inst.sid)))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn upgrade_until<T>(
    inst: &Instance,
    from: Instant,
    probe: impl Fn() -> T,
    done: impl Fn(&T) -> bool,
) -> T {
    let mut waits = version_waits(inst);
    let mut deadline = from + UPGRADE_WITHIN;
    loop {
        let got = probe();
        if done(&got) {
            return got;
        }
        let now = version_waits(inst);
        if now > waits {
            waits = now;
            deadline = deadline
                .max(Instant::now() + UPGRADE_WITHIN)
                .min(from + UPGRADE_CEILING);
        }
        if Instant::now() >= deadline {
            return got;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn an_activation_notice_upgrades_the_session_at_its_idle_point() {
    let inst = boot_upgrade(
        "v",
        "relaunch = false\nupgrade_ask = true\n",
        Some(SHORT_SETTLE_S),
    );
    let current = stage_upgrade_fake(&inst, "2.1.281", true);
    let newer = stage_upgrade_fake(&inst, "2.1.282", true);
    install(&inst, &current);
    let sel = format!("@{}", inst.sid);
    let tmp = inst.tmp.display();
    ctl_ok(&inst, &[&sel, "send", "/bin/bash --norc --noprofile -i"]);
    ctl_ok(&inst, &[&sel, "key", "enter"]);
    program_is(&inst, "bash", HANG);
    let line = format!(
        "cd {tmp}/work && '{}' --model opus",
        fake_path(&inst.tmp).display()
    );
    ctl_ok(&inst, &[&sel, "send", &line]);
    ctl_ok(&inst, &[&sel, "key", "enter"]);
    program_is(&inst, "claude", HANG);
    until(
        HANG,
        "the host attached",
        || status(&inst),
        |s| field(s, "supervisor").is_some_and(|v| v.starts_with("aterm-harness")),
    );
    let first = first_launch(&inst);
    assert_eq!(
        first.iter().map(|(_, a)| a.as_str()).collect::<Vec<_>>(),
        ["2.1.281 --model opus"],
        "{first:?}"
    );
    until(
        HANG,
        "the activation wake armed",
        || harness_lines(&inst),
        |l| l.contains("harness: the activation wake is armed"),
    );
    land(&inst, &newer);
    let got = upgrade_until(
        &inst,
        Instant::now(),
        || launches(&inst),
        |got| got.len() >= 2,
    );
    assert_eq!(
        got.len(),
        2,
        "relaunched once: {got:?}\nharness log:\n{}\njournal:\n{}",
        harness_lines(&inst),
        journal(&inst)
    );
    assert_ne!(got[0].0, got[1].0, "a new process");
    assert_eq!(
        got[1].1,
        format!("2.1.282 --model opus --resume {CONVERSATION}"),
        "the new build, its flags kept, its conversation resumed"
    );
    until(
        HANG,
        "the host handed on atpkg's notice",
        || harness_lines(&inst),
        |l| l.contains("harness: an activation notice — "),
    );
    probed_once(&inst);
    until(
        HANG,
        "announced, then told to carry on",
        || harness_lines(&inst),
        |l| {
            l.contains("step=announced:1")
                && (l.contains("step=done") || l.contains("step=continued"))
        },
    );
    program_is(&inst, "claude", HANG);
    until(
        HANG,
        "the tab's upgrade= column, headless",
        || status(&inst),
        |s| field(s, "upgrade").is_some_and(|v| v.starts_with("done/2.1.282/")),
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_session_nobody_asked_anything_is_restarted_afresh_with_nothing_typed() {
    let inst = boot_upgrade_on_the_shipped_clock("k", "relaunch = false\n", Some(SHORT_SETTLE_S));
    let current = stage_upgrade_fake(&inst, "2.1.281", false);
    let newer = stage_upgrade_fake(&inst, "2.1.282", false);
    install(&inst, &current);
    let sel = format!("@{}", inst.sid);
    let tmp = inst.tmp.display();
    ctl_ok(&inst, &[&sel, "send", "/bin/bash --norc --noprofile -i"]);
    ctl_ok(&inst, &[&sel, "key", "enter"]);
    program_is(&inst, "bash", HANG);
    let line = format!(
        "cd {tmp}/work && '{}' --model opus",
        fake_path(&inst.tmp).display()
    );
    ctl_ok(&inst, &[&sel, "send", &line]);
    ctl_ok(&inst, &[&sel, "key", "enter"]);
    program_is(&inst, "claude", HANG);
    until(
        HANG,
        "the host attached",
        || status(&inst),
        |s| field(s, "supervisor").is_some_and(|v| v.starts_with("aterm-harness")),
    );
    let first = first_launch(&inst);
    assert_eq!(
        first.iter().map(|(_, a)| a.as_str()).collect::<Vec<_>>(),
        ["2.1.281 --model opus"],
        "{first:?}"
    );
    land(&inst, &newer);
    let got = upgrade_until(
        &inst,
        Instant::now(),
        || launches(&inst),
        |got| got.len() >= 2,
    );
    assert_eq!(
        got.len(),
        2,
        "restarted once: {got:?}\nharness log:\n{}\njournal:\n{}",
        harness_lines(&inst),
        journal(&inst)
    );
    assert_ne!(got[0].0, got[1].0, "a new process");
    assert_eq!(
        got[1].1, "2.1.282 --model opus",
        "the new build, its flags kept, nothing resumed"
    );
    probed_once(&inst);
    let log = until(
        HANG,
        "restarted afresh",
        || harness_lines(&inst),
        |l| l.contains("step=done:fresh"),
    );
    assert!(!log.contains("announced"), "no notice: {log}");
    program_is(&inst, "claude", HANG);
    until(
        HANG,
        "the tab's upgrade= column, headless",
        || status(&inst),
        |s| field(s, "upgrade").is_some_and(|v| v.starts_with("done/2.1.282/")),
    );
    let typed = std::fs::read_to_string(inst.tmp.join("typed.log")).unwrap_or_default();
    assert!(typed.is_empty(), "typed into the agent: {typed:?}");
}

#[cfg(target_os = "macos")]
fn a_fresh_session_behind(
    tag: &str,
    shell: &str,
    harness: &str,
    settle_s: Option<u64>,
) -> Instance {
    let inst = boot_upgrade(tag, harness, settle_s);
    let current = stage_upgrade_fake(&inst, "2.1.281", false);
    let newer = stage_upgrade_fake(&inst, "2.1.282", false);
    install(&inst, &current);
    let sel = format!("@{}", inst.sid);
    let tmp = inst.tmp.display();
    let interactive = match shell {
        "zsh" => "/bin/zsh -f -i",
        _ => "/bin/bash --norc --noprofile -i",
    };
    ctl_ok(&inst, &[&sel, "send", interactive]);
    ctl_ok(&inst, &[&sel, "key", "enter"]);
    program_is(&inst, shell, HANG);
    let line = format!(
        "cd {tmp}/work && '{}' --model opus",
        fake_path(&inst.tmp).display()
    );
    ctl_ok(&inst, &[&sel, "send", &line]);
    ctl_ok(&inst, &[&sel, "key", "enter"]);
    program_is(&inst, "claude", HANG);
    let first = first_launch(&inst);
    assert_eq!(
        first.iter().map(|(_, a)| a.as_str()).collect::<Vec<_>>(),
        ["2.1.281 --model opus"],
        "{first:?}"
    );
    land(&inst, &newer);
    inst
}

#[cfg(target_os = "macos")]
fn typed_by(inst: &Instance) -> Vec<(u32, String)> {
    std::fs::read_to_string(inst.tmp.join("typed.log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (pid, line) = l.split_once(' ')?;
            Some((pid.parse().ok()?, line.to_string()))
        })
        .collect()
}

#[cfg(target_os = "macos")]
#[test]
fn a_prompt_sent_while_the_restart_holds_the_tab_lands_in_the_new_agent() {
    std::thread::scope(|scope| {
        let runs: Vec<_> = [("hb", "bash", true), ("hz", "zsh", false)]
            .into_iter()
            .map(|(tag, shell, turn)| scope.spawn(move || prompt_meets_the_hand(tag, shell, turn)))
            .collect();
        for run in runs {
            run.join().expect("a run");
        }
    });
}

#[cfg(target_os = "macos")]
fn hand_evidence(inst: &Instance) -> String {
    let look = |args: &[&str]| String::from_utf8_lossy(&ctl(inst, args).stdout).into_owned();
    let sel = format!("@{}", inst.sid);
    format!(
        "status: {}\nscreen:\n{}\nlaunches: {:?}\nharness log:\n{}\njournal:\n{}\n\
         processes under the instance (pid ppid stat wchan etime command):\n{}",
        look(&[&sel, "status"]),
        look(&[&sel, "text"]),
        launches(inst),
        harness_lines(inst),
        journal(inst),
        headless_boot::process_tree(inst)
    )
}

#[cfg(target_os = "macos")]
fn prompt_meets_the_hand(tag: &str, shell: &str, turn: bool) {
    let inst = a_fresh_session_behind(tag, shell, "relaunch = false\n", Some(SHORT_SETTLE_S));
    let sel = format!("@{}", inst.sid);
    let pwned = inst.tmp.join("work/pwned");
    let prompt = format!("Fix the parser; touch {}", pwned.display());
    let gate = inst.tmp.join("hold-idle-2.1.282");
    std::fs::write(&gate, "").expect("the gate");
    let hand_on =
        |s: &String| field(s, "hand").is_some_and(|h| h.starts_with("lease:aterm-harness@"));
    let st = upgrade_until(&inst, Instant::now(), || status(&inst), hand_on);
    assert!(
        hand_on(&st),
        "{shell}: the restart's hand on the tab: {st}\n{}",
        hand_evidence(&inst)
    );

    let deadline = Instant::now() + HANG;
    let mut held = 0;
    let mut until_taken = |args: &[&str]| loop {
        let out = ctl(&inst, args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !stderr.contains("ERR busy") {
            assert!(
                out.status.success(),
                "{shell}: {args:?}: {out:?}\n{}",
                hand_evidence(&inst)
            );
            break;
        }
        if stderr.contains("ERR busy lease=aterm-harness@") {
            held += 1;
            let _ = std::fs::remove_file(&gate);
        }
        assert!(
            Instant::now() < deadline,
            "{shell}: never let in\n{}",
            hand_evidence(&inst)
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    if turn {
        let hang_ms = format!("timeout={}", HANG.as_millis());
        until_taken(&[&sel, "turn", "idle=1", &hang_ms, &prompt]);
    } else {
        until_taken(&[&sel, "send", &prompt]);
        until_taken(&[&sel, "key", "enter"]);
    }
    assert!(held >= 1, "{shell}: the hand held the tab");
    let log = until(
        HANG,
        "restarted afresh",
        || harness_lines(&inst),
        |l| l.contains("step=done:fresh"),
    );
    let got = launches(&inst);
    assert_eq!(got.len(), 2, "{shell}: {got:?}\n{log}");
    until(
        HANG,
        "the prompt in the relaunched agent",
        || format!("{:?}", typed_by(&inst)),
        |_| {
            typed_by(&inst)
                .iter()
                .any(|(pid, line)| *pid == got[1].0 && *line == prompt)
        },
    );
    assert!(!pwned.exists(), "{shell}: the shell ran the prompt");
    assert!(
        !typed_by(&inst).iter().any(|(pid, _)| *pid == got[0].0),
        "{shell}: the old process read nothing: {:?}",
        typed_by(&inst)
    );
}

#[test]
fn a_wall_the_host_resumes_is_no_stop_and_one_nobody_answers_is() {
    std::thread::scope(|scope| {
        let runs: Vec<_> = [("wn", "", false), ("wo", "resume_limits = false\n", true)]
            .into_iter()
            .map(|(tag, harness, stop)| scope.spawn(move || wall_level(tag, harness, stop)))
            .collect();
        for run in runs {
            run.join().expect("a run");
        }
    });
}

fn wall_level(tag: &str, harness: &str, stop: bool) {
    let inst = boot(tag, harness);
    run_fake(&inst, "k1", &["wall.hold"; 12]);
    let st = until(
        HANG,
        "the wall is published under the harness's claim",
        || status(&inst),
        |s| s.contains("agent=wall:usage-weekly") && s.contains("supervisor=aterm-harness@"),
    );
    let st = if stop {
        st
    } else {
        until(
            HANG,
            "the level leaves the stop",
            || status(&inst),
            |s| s.contains("agent=wall:usage-weekly") && !s.contains("level=limited"),
        )
    };
    assert_eq!(
        st.contains("level=limited"),
        stop,
        "{tag}: {st}\n{}",
        harness_lines(&inst)
    );
}

fn contents(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                out.push((format!("{}/", path.display()), Vec::new()));
                stack.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap_or_default();
                out.push((path.display().to_string(), bytes));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn contents_sees_an_empty_directory() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-contents-")
        .tempdir()
        .expect("a dir");
    assert!(contents(dir.path()).is_empty());
    std::fs::create_dir_all(dir.path().join("a").join("b")).expect("made");
    let seen: Vec<String> = contents(dir.path()).into_iter().map(|(p, _)| p).collect();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(seen.iter().all(|p| p.ends_with('/')), "{seen:?}");
}

fn pressed(report: &aterm_agent::supervise::tape::report::Report) -> Vec<String> {
    report
        .requests
        .iter()
        .filter(|words| {
            words
                .iter()
                .find(|w| !w.starts_with('@'))
                .is_some_and(|v| v == "key")
        })
        .filter_map(|words| words.last().cloned())
        .collect()
}

fn tape_state(tmp: &Path) -> String {
    std::fs::canonicalize(tmp.join("state"))
        .expect("the scratch state root")
        .to_str()
        .expect("a UTF-8 scratch path")
        .to_string()
}

fn listed_tapes(inst: &Instance) -> Vec<PathBuf> {
    let sel = format!("@{}", inst.sid);
    let state = tape_state(&inst.tmp);
    let out = headless_boot::drive_in(
        &inst.tmp,
        &["tape", "ls", &sel],
        &[("ATERM_STATE_HOME", &state)],
        None,
    );
    assert_eq!(out.status.code(), Some(0), "{}", headless_boot::said(&out));
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once(" ago  ").map(|(_, rest)| rest))
        .map(|rest| PathBuf::from(rest.trim_end_matches("  kept")))
        .collect()
}

fn tape_ended(path: &Path) -> bool {
    use aterm_agent::supervise::tape::codec::Item;
    std::fs::read(path)
        .ok()
        .and_then(|raw| aterm_agent::supervise::tape::show::open(&raw, false).ok())
        .is_some_and(|tape| tape.items.iter().any(|i| matches!(i, Item::End(_))))
}

#[test]
fn a_hosted_run_is_taped_and_replays_offline_in_a_changed_world() {
    use aterm_agent::supervise::tape::replay::{Options, replay};
    use aterm_agent::supervise::tape::report::Verdict;
    use aterm_agent::supervise::tape::store::HOSTED_SEAM;

    let inst = boot_on("tape", "", 40, None, true, |tmp| {
        vec![
            (HOSTED_SEAM, "1".to_string()),
            ("ATERM_STATE_HOME", tape_state(tmp)),
        ]
    });
    run_fake(&inst, "kt", &["touch.txt"]);
    keys_until(&inst, "kt", &["1"], HANG);
    program_is(&inst, "sh", HANG);
    until(
        HANG,
        "the host's loop gave its claim back",
        || status(&inst),
        |s| field(s, "supervisor") == Some("-"),
    );
    let deadline = Instant::now() + HANG;
    let tape = loop {
        let tapes = listed_tapes(&inst);
        if let [tape] = tapes.as_slice()
            && tape_ended(tape)
        {
            break tape.clone();
        }
        assert!(
            Instant::now() < deadline,
            "one ended tape of the run: not within {HANG:?}; listed {tapes:?}\nharness log:\n{}",
            harness_lines(&inst)
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let session_dir = tape.parent().expect("the session's directory");
    assert_eq!(
        session_dir.file_name().and_then(|n| n.to_str()),
        Some(inst.sid.as_str()),
        "{}",
        tape.display()
    );
    let tapes_dir = session_dir.parent().expect("the store");
    assert!(tapes_dir.ends_with("drive/tapes"), "{}", tape.display());
    let drive = tapes_dir.parent().expect("<state>/drive").to_path_buf();
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&tape)
            .expect("the tape")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the tape is private: {mode:o}");
    }
    let before = contents(&drive);
    assert!(
        before.iter().any(|(p, _)| p.ends_with(".journal.jsonl")),
        "the run journaled beside its tape: {:?}",
        before.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
    let bytes = std::fs::read(&tape).expect("the tape");

    let started = Instant::now();
    let report = replay(&bytes, &Options::default()).expect("the tape replays");
    let took = started.elapsed();
    assert_eq!(report.verdict, Verdict::Reproduced, "{}", report.render());
    assert_eq!(report.consumed, report.effects, "{}", report.render());
    assert_eq!(
        pressed(&report),
        keys(&inst, "kt"),
        "the tape's acts are what reached the fake: {}",
        report.render()
    );
    eprintln!(
        "measured, not judged: {} effects of a `{}` tape replayed in {took:?} in-process",
        report.effects, report.entry
    );

    let tape_s = tape.to_str().expect("a UTF-8 scratch path");
    let same = headless_boot::drive_in(&inst.tmp, &["replay", tape_s], &[], None);
    assert_eq!(
        same.status.code(),
        Some(0),
        "{}",
        headless_boot::said(&same)
    );
    assert!(
        String::from_utf8_lossy(&same.stdout).starts_with("REPRODUCED"),
        "{}",
        headless_boot::said(&same)
    );

    let elsewhere = aterm_tempfile::Builder::new()
        .prefix("aterm-elsewhere-")
        .tempdir()
        .expect("another world");
    let root = elsewhere.path();
    std::fs::create_dir_all(root.join("tmp")).expect("its temp dir");
    let changed = headless_boot::drive_in(
        root,
        &["replay", tape_s],
        &[("HOME", "/nonexistent"), ("TZ", "Pacific/Kiritimati")],
        Some(root),
    );
    assert_eq!(
        changed.status.code(),
        Some(0),
        "{}",
        headless_boot::said(&changed)
    );
    assert!(
        String::from_utf8_lossy(&changed.stdout).starts_with("REPRODUCED"),
        "{}",
        headless_boot::said(&changed)
    );
    let made = contents(&root.join("tmp"));
    assert!(
        made.is_empty(),
        "the replay made nothing in its temp dir: {:?}",
        made.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
    assert!(
        before == contents(&drive),
        "the replays touched none of the recording's files"
    );
}
