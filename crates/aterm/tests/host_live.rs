// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless conformance for the IN-GUI SUPERVISOR HOST (`aterm-gui`'s
//! `harness_host`): a headless instance supervises every Claude Code session
//! it owns by default, with nothing started by hand — no `aterm supervise`,
//! no command typed for it.
//!
//! The worker is a FAKE `claude`: a `#!/bin/sh` script named `claude` (so the
//! server publishes `program=claude`, the shim rule of `session_program`)
//! that draws aterm-phase's measured Claude Code 2.1.280/2.1.282 screens one
//! after another, waits for ONE key after each — one byte, or an escape and
//! the two bytes after it, so an arrow is one key — and appends it to a key
//! log (`1`, `enter`, `up`, `down`: [`keys`]); a screen named `*.hold` is
//! shown for a few seconds and reads nothing. The key log is the ground truth
//! of what the host typed.
//!
//! * THE HOST ATTACHES: its claim (`meta-change field=supervisor`) is on the
//!   session's timeline within 2 s of the server's first agent verdict for
//!   the fake — two server stamps, read after the press.
//! * BY DEFAULT EVERY BOX IS ANSWERED (the owner's direction of
//!   2026-09-24): the rm circuit breaker over `S=/usr` gets `1`, and so does
//!   a `touch x` box; a 49-row screen's tall box is read whole.
//! * AND EVERY QUESTION WITH ITS RECOMMENDED OPTION (`[harness]
//!   answer_questions`, the owner's directive of 2026-09-25): the four-tab
//!   dialog of that day's incident gets Enter on each tab's `(Recommended)`
//!   option and on its review's `Submit answers` — never a digit; a focus on
//!   the free-text row is moved up, one row a key, before the Enter; a
//!   question a person typed into is handed to them, and so is every
//!   question under `answer_questions = false`, with nothing typed. A
//!   question in a shape 2.1.282 does not draw (the hand-built column-1
//!   `Yes`/`No`) is handed over as one aterm-phase did not read whole.
//! * WHAT THE SAFE RULES PROVE KEEPS ITS RULE: a read-only Bash box gets `1`;
//!   the session survey `0`; the rm circuit breaker in a bypass session over
//!   a scratch path `1`.
//! * UNDER `approve = "safe"`, WHAT THEY CANNOT PROVE IS HANDED TO THE
//!   HUMAN: the same breaker over `S=/usr` and a `touch x` box leave the
//!   attention set, naming the
//!   command, and nothing typed; switching `[harness] enabled` off in the file
//!   stops the supervisor (its claim and its badge go) and still types nothing.
//! * ONE SUPERVISOR PER SESSION: another holder's live claim keeps the host
//!   off the session; it attaches once that claim lapses.
//! * NEGATIVE CONTROL: the same headless instance under `headless = false`
//!   (the one key that takes a headless instance's supervision away; the
//!   default supervises it) runs no host — the box waits, `supervisor=-`,
//!   nothing typed.
//!
//! * A BOX A PERSON ANSWERED LOSES ITS BADGE AT ONCE: under `approve =
//!   "none"` the owner's own box (2026-09-24) is escalated; answered, it is
//!   cleared within 3 s while the worker is still busy — never at the next
//!   idle point (the 0.92.0 host's silent loop).
//!
//! * AN AGENT THAT EXITS UNASKED IS RELAUNCHED: a fake that dies by SIGKILL
//!   is started again in its tab on its own conversation (`--resume <id>`,
//!   its launch flags kept), handed to a supervisor at once, and given its
//!   continuation at that supervisor's loop's first idle point; NEGATIVE CONTROL:
//!   under `[harness] relaunch = false` the next death is left alone.
//!
//! * THE LIVE UPGRADE IS A STEP OF THE WORKER: atpkg's activation notice
//!   (a push, no sweep) asks the loop for its next idle point, where the worker
//!   announces, reads READY, ends the agent and relaunches it on the new
//!   build on its conversation, and carries it on.
//!
//! * AN AGENT WHOSE EXIT WAS ITS OWN END IS LEFT ALONE: every fake above
//!   exits when its screens are done, under the full-power default
//!   (`relaunch` and `upgrade` on) — never having registered a conversation,
//!   its exit is journaled and neither relaunched nor said.
//!
//! ISOLATION: scratch HOME/XDG roots, a private control socket, a config with
//! every automatic lane off but the supervisor under test (its `[harness]`
//! at the full-power default, the host's own acts included), `--no-reroute`, `SHELL=/bin/sh`
//! (`support/launch_isolation.rs`). A headless instance never reaches
//! WindowServer, and the fake is typed by its absolute path so no real agent
//! on the machine's PATH can start.

#![cfg(unix)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_isolation.rs"]
mod launch_isolation;

const SOCKET_POLLS: usize = 300;
const POLL_GAP: Duration = Duration::from_millis(100);
const CLIENT_EXIT_DEADLINE: Duration = Duration::from_secs(60);
const MAX_SOCK_PATH: usize = 100;
/// How long a `*.hold` screen stays up: long enough for the supervisor to
/// read it settled (its footer is what says bypass).
const HOLD_S: u32 = 5;
/// How long "nothing typed" is watched for after the escalation showed.
const QUIET: Duration = Duration::from_secs(3);

/// One booted headless instance plus its scratch world, torn down on every
/// exit path (Drop runs on panic too).
struct Instance {
    child: Child,
    /// Cut after `child` is killed (fields drop after `Drop::drop`), and closed by
    /// the kernel if this test process dies first: the instance goes with it.
    _lifeline: aterm_uds::lifeline::Lifeline,
    tmp: PathBuf,
    log: PathBuf,
    sock: String,
    sid: String,
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

fn log_tail(log: &Path) -> String {
    let body = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = body.lines().collect();
    let start = lines.len().saturating_sub(15);
    lines[start..].join("\n")
}

fn is_socket_or_symlink(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_socket() || m.file_type().is_symlink())
        .unwrap_or(false)
}

fn scratch_root(tag: &str) -> Option<PathBuf> {
    let name = format!("athh{tag}-{}", std::process::id());
    for base in [std::env::temp_dir(), PathBuf::from("/tmp")] {
        let tmp = base.join(&name);
        let sock = tmp.join("run/aterm/aterm.sock");
        if sock.as_os_str().len() >= MAX_SOCK_PATH {
            continue;
        }
        if launch_isolation::prepare(&tmp).is_err() {
            let _ = std::fs::remove_dir_all(&tmp);
            continue;
        }
        return Some(tmp);
    }
    None
}

/// Boot one headless instance, 40x200, its `[harness]` table the keys
/// `harness` in place of the isolation config's (which switches the
/// supervisor off). `None` = an environmental refusal, announced as a SKIP
/// with the log tail.
fn boot(tag: &str, harness: &str) -> Option<Instance> {
    boot_sized(tag, harness, 40)
}

/// [`boot`] with `lines` rows.
fn boot_sized(tag: &str, harness: &str, lines: u16) -> Option<Instance> {
    let Some(tmp) = scratch_root(tag) else {
        eprintln!("SKIP: no scratch base with a short enough socket path");
        return None;
    };
    let cfg = tmp.join("cfg/aterm/aterm.toml");
    let base = std::fs::read_to_string(&cfg).expect("the isolation config");
    assert!(base.contains(launch_isolation::HARNESS_OFF), "{base}");
    let base = base.replace(launch_isolation::HARNESS_OFF, "");
    std::fs::write(&cfg, format!("{base}[harness]\n{harness}")).expect("write the config");
    write_world(&tmp);
    let log = tmp.join("gui.log");
    let (out, err) = match std::fs::File::create(&log).and_then(|f| Ok((f.try_clone()?, f))) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("SKIP: cannot open the instance log ({e})");
            let _ = std::fs::remove_dir_all(&tmp);
            return None;
        }
    };
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    launch_isolation::apply(&mut cmd, &tmp);
    cmd.args(["--headless", launch_isolation::NO_REROUTE])
        .args(launch_isolation::control_sock(&tmp))
        // The supervisor's lines (`harness @<sid>: …`) are the diagnosis a
        // failed assertion prints ([`harness_lines`]).
        // (A development seam: this is a debug build.)
        .env("ATERM_LOG", "info")
        // The fixtures are 120-140 columns wide; Claude Code lays out its own
        // rows, so a terminal wrap is not a shape it draws.
        .args(["--lines", &lines.to_string(), "--columns", "200"])
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    let lifeline = launch_isolation::lifeline(&mut cmd, &tmp);
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP: cannot launch aterm --headless ({e})");
            let _ = std::fs::remove_dir_all(&tmp);
            return None;
        }
    };
    let sock_path = tmp.join("run/aterm/aterm.sock");
    let mut inst = Instance {
        child,
        _lifeline: lifeline,
        sock: sock_path.to_string_lossy().into_owned(),
        tmp,
        log,
        sid: String::new(),
    };
    for _ in 0..SOCKET_POLLS {
        if matches!(inst.child.try_wait(), Ok(Some(_)) | Err(_)) {
            eprintln!(
                "SKIP: aterm --headless exited before binding its socket; log tail:\n{}",
                log_tail(&inst.log)
            );
            return None;
        }
        if is_socket_or_symlink(&sock_path) && launch_isolation::control_listening(&sock_path) {
            inst.sid = boot_session(&inst);
            return Some(inst);
        }
        std::thread::sleep(POLL_GAP);
    }
    eprintln!(
        "SKIP: control socket never started listening; log tail:\n{}",
        log_tail(&inst.log)
    );
    None
}

fn client_command(inst: &Instance, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    cmd.arg("ctl")
        .arg("--sock")
        .arg(&inst.sock)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    launch_isolation::apply(&mut cmd, &inst.tmp);
    cmd
}

/// One bounded `aterm ctl --sock <sock> <args…>` call.
fn ctl(inst: &Instance, args: &[&str]) -> Output {
    let mut child = client_command(inst, args).spawn().expect("spawn aterm ctl");
    fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        })
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + CLIENT_EXIT_DEADLINE;
    loop {
        match child.try_wait().expect("poll aterm ctl") {
            Some(status) => {
                return Output {
                    status,
                    stdout: stdout.join().expect("stdout drain"),
                    stderr: stderr.join().expect("stderr drain"),
                };
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("aterm ctl {args:?} did not exit in time");
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn ctl_ok(inst: &Instance, args: &[&str]) -> String {
    let out = ctl(inst, args);
    assert!(
        out.status.success(),
        "aterm ctl {args:?} failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).expect("utf-8")
}

/// `key=value` out of a status/meta line.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    line.split_whitespace()
        .find_map(|t| t.strip_prefix(prefix.as_str()))
}

/// Undo the wire's pct-encoding (`%20` and friends).
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

/// The boot session's sid, from `sessions`.
fn boot_session(inst: &Instance) -> String {
    let body = ctl_ok(inst, &["sessions"]);
    let row = body
        .lines()
        .find(|l| l.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or_else(|| panic!("a session row: {body}"));
    row.split_whitespace().nth(1).expect("a sid").to_string()
}

fn status(inst: &Instance) -> String {
    ctl_ok(inst, &[&format!("@{}", inst.sid), "status"])
}

fn meta(inst: &Instance) -> String {
    ctl_ok(inst, &[&format!("@{}", inst.sid), "meta"])
}

/// The session's attention text, decoded (`None` for `-`).
fn attention(inst: &Instance) -> Option<String> {
    let m = meta(inst);
    field(&m, "attention").filter(|v| *v != "-").map(pct_decode)
}

/// The supervisor host's lines from the instance's log (under its scratch
/// home), for a failure's message.
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

/// The first file named `name` under `dir`, at any depth.
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

/// Poll `probe` until `pred` holds, returning the matching record, or panic
/// with the last record after `within`.
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

/// The boot session's timeline records after id `after`, as `(id, t_ms, kind,
/// payload)`. `t_ms` is the SERVER's clock, stamped as each record was made,
/// so two of them bound a latency with no client process between them.
fn timeline_since(inst: &Instance, after: u64) -> Vec<(u64, u64, String, String)> {
    let since = format!("since={after}");
    ctl_ok(inst, &[&format!("@{}", inst.sid), "timeline", &since])
        .lines()
        .filter_map(|l| {
            let (id, rest) = l.strip_prefix("event ")?.split_once(" t=")?;
            let (t, rest) = rest.split_once(" kind=")?;
            let (kind, payload) = rest.split_once(' ').unwrap_or((rest, ""));
            Some((
                id.parse().ok()?,
                t.parse().ok()?,
                kind.to_string(),
                payload.to_string(),
            ))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The fake worker and its screens.
// ---------------------------------------------------------------------------

/// A fixture without its provenance line.
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
const SPINNER: &str = include_str!("../../aterm-phase/src/fixtures/spinner-tempering.txt");
const OFFER: &str = include_str!("../../aterm-phase/src/fixtures/hand-built-offer-end-of-turn.txt");
/// The owner's box (2026-09-24, live): a workflow subagent's rm-breaker box.
const OWNERS_BOX: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.28x-box-rm-workflow.txt");
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

/// A session just started in bypass mode: the measured 2.1.280 banner over
/// an empty composer and the `⏵⏵ bypass permissions on` footer (the offer
/// fixture's last four rows). Idle with no turn behind it, so nothing is
/// owed to it — and the footer is read at the supervisor's first look.
/// (A busy screen is not enough: the loop waits a busy turn out without
/// re-reading it, so the footer it carried is never seen.)
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

/// The measured rm-breaker box over `cmd` instead of the loop it was
/// measured with (which escalates by construction: `set` in a loop).
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

/// A workflow's Bash box whose heredoc has `lines` rows (aterm-phase's
/// hand-built `tall_bash_box` shape, from the measured incident box): with
/// 33, the box is 43 rows — whole on a 49-row screen, its title more than 40
/// rows above its footer.
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

/// `screen` with the `❯` focus moved from the row it is on onto the row
/// that reads `to` (both drawn at column 2, as 2.1.282 draws its options).
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

/// `screen` from its dialog's top rule down: the dialog drawn at the top of
/// the pane.
fn from_the_rule(screen: &str) -> String {
    let rows: Vec<&str> = screen.lines().collect();
    let rule = rows
        .iter()
        .position(|r| r.starts_with('─'))
        .expect("the dialog's rule");
    rows[rule..].join("\n") + "\n"
}

/// Where the fake `claude` lives: the scratch HOME's managed atpkg prefix
/// (`atpkg::store::default_prefix`), at the twin's own path `agents/claude` —
/// the only agent-named script the server reads as the agent before it
/// execs (a `/bin/sh` script named `claude` anywhere else is a shell).
fn fake_path(tmp: &Path) -> PathBuf {
    atpkg::store::default_prefix(&tmp.join("home")).join("agents/claude")
}

/// Write the fake `claude` and every screen under `tmp`.
fn write_world(tmp: &Path) {
    use std::os::unix::fs::PermissionsExt;
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
         # as `od -c` spells it; a *.hold screen is shown and nothing read.\n\
         log=$1; shift\n\
         old=$(stty -g)\n\
         stty -icanon -echo min 1\n\
         for scene in \"$@\"; do\n\
         \x20 printf '\\033[2J\\033[H'\n\
         \x20 cat \"$scene\"\n\
         \x20 case \"$scene\" in *.hold) sleep {HOLD_S}; continue ;; esac\n\
         \x20 k=$(dd bs=1 count=1 2>/dev/null | od -An -c | tr -d ' ')\n\
         \x20 case \"$k\" in 033) k=\"$k$(dd bs=1 count=2 2>/dev/null | od -An -c | tr -d ' ')\" ;; esac\n\
         \x20 printf '%s\\n' \"$k\" >> \"$log\"\n\
         done\n\
         stty \"$old\"\n\
         printf '\\033[2J\\033[H'\n"
    );
    std::fs::write(&claude, fake).expect("write the fake");
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    // The measured touch box draws its third option garbled (`3. Nooject`,
    // aterm-phase's `the_touch_box_is_bash_whatever_its_description_says`),
    // and decision 1's rules press a one-shot `Yes` only on a box that also
    // offers a readable `No` (`once_choice`): the read box repairs that row,
    // so what it tests is the read-only rule, not the garble. (`touch.txt`
    // keeps it: full power presses it as measured, and the safe rules
    // escalate it either way.)
    let read = body(BOX_TOUCH)
        .replace("touch x", "ls -la")
        .replace("Create file x", "List the files")
        .replace("3. Nooject", "3. No");
    // The measured survey held a draft in the composer, and a supervisor
    // never presses over a draft: the survey with the composer empty.
    let survey = body(SURVEY).replace(
        "❯ Fix the parser path break yourself, same rules as the last one",
        "❯ ",
    );
    let screens = [
        ("read.txt", read),
        ("touch.txt", body(BOX_TOUCH)),
        ("question.txt", body(QUESTION)),
        ("survey.txt", survey),
        ("busy.hold", body(SPINNER)),
        ("bypass.hold", bypass_start()),
        ("rm-scratch.txt", rm_box("S=/tmp/athh-scratch; rm -rf $S/a")),
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
}

/// Start the fake on `screens` in the boot session, from `work/` with the
/// cwd reported as shell integration reports it (OSC 7), and wait until the
/// server names its program.
fn run_fake(inst: &Instance, keys: &str, screens: &[&str]) {
    let tmp = inst.tmp.display();
    let scenes: Vec<String> = screens.iter().map(|s| format!("{tmp}/sc/{s}")).collect();
    let line = format!(
        "clear; cd {tmp}/work; printf '\\033]7;file://localhost%s\\007' \"$PWD\"; \
         '{}' {tmp}/{keys} {}",
        fake_path(&inst.tmp).display(),
        scenes.join(" ")
    );
    let sel = format!("@{}", inst.sid);
    ctl_ok(inst, &[&sel, "send", &line]);
    ctl_ok(inst, &[&sel, "key", "enter"]);
}

/// The keys the fake read, one per line, named: `od -c`'s `\r` (or `\n`,
/// the tty's CR-to-NL) is `enter`, `033[A`/`033OA` (normal or application
/// cursor keys) `up`, `033[B`/`033OB` `down`; anything else as read.
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

/// The approvals ledger the host wrote for the boot session
/// (`<state>/drive/<sid>.jsonl`, under the scratch world).
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

fn keys_until(inst: &Instance, log: &str, want: &[&str], within: Duration) {
    // The attention rides along in the record, so a timeout says why the
    // host handed the box over instead of answering it.
    let deadline = Instant::now() + within;
    let mut last = String::new();
    while Instant::now() < deadline {
        let got = keys(inst, log);
        if got == want {
            return;
        }
        last = format!("{got:?} attention={:?}", attention(inst));
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "the fake read {want:?}: not within {within:?}; last: {last}\nharness log:\n{}",
        harness_lines(inst)
    );
}

/// Wait for the session's program to be `want` (the fake started or ended).
fn program_is(inst: &Instance, want: &str, within: Duration) -> String {
    until(
        within,
        &format!("program={want}"),
        || status(inst),
        |s| field(s, "program") == Some(want),
    )
}

/// The attention appears naming `needle`; then for [`QUIET`] nothing is
/// typed; the attention stands throughout.
fn escalated_and_quiet(inst: &Instance, log: &str, needle: &str) -> String {
    let text = until(
        Duration::from_secs(20),
        &format!("an attention naming {needle:?}"),
        || attention(inst).unwrap_or_default(),
        |a| a.contains(needle),
    );
    let quiet_until = Instant::now() + QUIET;
    while Instant::now() < quiet_until {
        assert!(keys(inst, log).is_empty(), "typed: {:?}", keys(inst, log));
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        attention(inst).is_some_and(|a| a.contains(needle)),
        "the attention stands"
    );
    text
}

/// THE OWNER'S DEFAULT (2026-09-24, and 2026-09-25 for questions): with no
/// `[harness]` key written, the host answers every box — the rm circuit
/// breaker over `S=/usr` in a bypass session, which the safe rules hand over
/// ([`the_host_hands_over_what_it_cannot_prove_and_types_nothing`]), and the
/// measured `touch x` box as captured, its garbled `3. Nooject` and all, get
/// their one-shot `Yes` — answers a live-geometry question (S1-01) with Enter
/// on its recommended option, never a digit, and hands the column-1 question
/// (a shape 2.1.282 does not draw) to the human with nothing typed: a
/// question aterm-phase did not read whole is never answered, and its
/// `Yes`/`No` is the question's, never a confirmation.
#[test]
fn the_host_answers_every_box_by_default() {
    let Some(inst) = boot("d", "") else {
        return;
    };
    run_fake(&inst, "k1", &["bypass.hold", "rm-usr.txt"]);
    keys_until(&inst, "k1", &["1"], Duration::from_secs(25));
    program_is(&inst, "sh", Duration::from_secs(10));

    run_fake(&inst, "k2", &["touch.txt"]);
    keys_until(&inst, "k2", &["1"], Duration::from_secs(15));
    program_is(&inst, "sh", Duration::from_secs(10));

    run_fake(&inst, "k3", &["q-one.txt"]);
    keys_until(&inst, "k3", &["enter"], Duration::from_secs(20));
    program_is(&inst, "sh", Duration::from_secs(10));

    run_fake(&inst, "k4", &["question.txt"]);
    let text = escalated_and_quiet(&inst, "k4", "claude question");
    assert!(text.contains("did not read whole"), "{text}");
}

/// ON A SCREEN TALLER THAN THE 40 ROWS THE WINDOW READS (the validate
/// drive of 2026-09-24, a private headless aterm at 130x49): the measured
/// Yes/No question drawn at the top of a 49-row screen reached the reader
/// cut at `3. Type something.`, and the parser's backwards body slice
/// panicked the window's main thread — the whole instance died — and a
/// 43-row Bash box whose title sat on row 3 was read from the supervisor's
/// last 40 rows as "taller than the pane" and escalated. Now a live-geometry
/// question drawn at the top of the 49 rows (S1-01 from its rule down: the
/// last 40 rows start inside its options) is read again whole and answered
/// with Enter, the column-1 one is handed over as a question with nothing
/// typed and the instance lives, and the tall box, read again whole, gets
/// `1`.
#[test]
fn a_49_row_screen_is_read_whole_and_the_instance_lives() {
    let Some(mut inst) = boot_sized("t", "", 49) else {
        return;
    };
    run_fake(&inst, "k0", &["q-one-top.txt"]);
    keys_until(&inst, "k0", &["enter"], Duration::from_secs(20));
    program_is(&inst, "sh", Duration::from_secs(10));

    run_fake(&inst, "k1", &["question.txt"]);
    let text = escalated_and_quiet(&inst, "k1", "claude question");
    assert!(text.contains("did not read whole"), "{text}");
    assert!(
        matches!(inst.child.try_wait(), Ok(None)),
        "the instance died; log tail:\n{}",
        log_tail(&inst.log)
    );
    // The handed-over question is the person's: their key ends the fake.
    ctl_ok(&inst, &[&format!("@{}", inst.sid), "key", "z"]);
    keys_until(&inst, "k1", &["z"], Duration::from_secs(10));
    program_is(&inst, "sh", Duration::from_secs(10));

    run_fake(&inst, "k2", &["tall.txt"]);
    keys_until(&inst, "k2", &["1"], Duration::from_secs(15));
}

/// THE INCIDENT OF 2026-09-25, ANSWERED LIVE: the four-tab dialog (S6-01 …
/// S6-05) gets Enter on each tab's `(Recommended)` option and on the
/// review's `Submit answers` — five Enters, never a digit — with no badge,
/// and the host's ledger holds five `answer-recommended@v1` approvals.
#[test]
fn the_host_answers_a_tabbed_question_with_its_recommended_options() {
    let Some(inst) = boot("q", "") else {
        return;
    };
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
        Duration::from_secs(30),
    );
    program_is(&inst, "sh", Duration::from_secs(10));
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

/// The free-text row focused (S1-03): the host moves the focus UP one row a
/// key — each move seen landing on the next screen — and presses Enter only
/// once it is on `1. Dark (Recommended)`. Never a digit, never Enter on the
/// free-text row.
#[test]
fn the_host_moves_off_the_free_text_row_before_answering() {
    let Some(inst) = boot("f", "") else {
        return;
    };
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
    keys_until(
        &inst,
        "k1",
        &["up", "up", "up", "enter"],
        Duration::from_secs(30),
    );
}

/// A person's text in the free-text row (S1-04): the question is theirs —
/// badged with the reason, nothing typed.
#[test]
fn the_host_hands_over_a_question_a_person_typed_into() {
    let Some(inst) = boot("p", "") else {
        return;
    };
    run_fake(&inst, "k1", &["q-typed.txt"]);
    let text = escalated_and_quiet(&inst, "k1", "claude question");
    assert!(text.contains("a person has begun answering"), "{text}");
}

/// The owner's switch: under `answer_questions = false` every question is
/// the human's — the incident's first tab is badged naming the switch, and
/// nothing is typed.
#[test]
fn answer_questions_false_hands_every_question_over() {
    let Some(inst) = boot("x", "answer_questions = false\n") else {
        return;
    };
    run_fake(&inst, "k1", &["q-tab1.txt"]);
    let text = escalated_and_quiet(&inst, "k1", "claude question");
    assert!(text.contains("answer_questions is off"), "{text}");
}

#[test]
fn the_host_answers_what_the_policy_proves_safe() {
    let Some(inst) = boot("a", "") else {
        return;
    };
    // A read-only Bash box: the host attaches within 2 s of the session being
    // published as Claude Code, and presses `1`. Both ends are the server's
    // own timeline records, read once the press has landed: `program=claude`
    // and the host's claim live only until the host answers and the fake
    // exits, and sampling each through an `aterm ctl status` spawn could miss
    // both (76dff586e's shape) — while the 2 s stopwatch timed the spawns too.
    let mark = timeline_since(&inst, 0).last().map_or(0, |e| e.0);
    run_fake(&inst, "k1", &["read.txt"]);
    keys_until(&inst, "k1", &["1"], Duration::from_secs(15));
    program_is(&inst, "sh", Duration::from_secs(10));
    let events = timeline_since(&inst, mark);
    let published = events
        .iter()
        .find(|e| e.2 == "agent-change")
        .unwrap_or_else(|| panic!("never published as an agent: {events:?}"))
        .1;
    let attached = events
        .iter()
        .find(|e| e.2 == "meta-change" && e.3.starts_with("field=supervisor value=aterm-harness"))
        .unwrap_or_else(|| {
            panic!(
                "the host never claimed the session: {events:?}\n{}",
                harness_lines(&inst)
            )
        })
        .1;
    let late = attached.saturating_sub(published);
    assert!(
        late <= 2000,
        "the host attached {late} ms after the session was published as an agent: {events:?}"
    );

    // The session survey, after a screen without it: dismissed with `0`.
    run_fake(&inst, "k2", &["busy.hold", "survey.txt"]);
    keys_until(&inst, "k2", &["0"], Duration::from_secs(25));
    program_is(&inst, "sh", Duration::from_secs(10));

    // The rm circuit breaker in a bypass session, every operand under a
    // scratch root: `1`.
    run_fake(&inst, "k3", &["bypass.hold", "rm-scratch.txt"]);
    keys_until(&inst, "k3", &["1"], Duration::from_secs(25));
    program_is(&inst, "sh", Duration::from_secs(10));
    // The host detached with the program: its claim went with it.
    until(
        Duration::from_secs(10),
        "supervisor=- after the agent left",
        || status(&inst),
        |s| field(s, "supervisor") == Some("-"),
    );
    // The fake's exit was its own end (it registered no conversation):
    // journaled, never relaunched — the tab's shell holds it — and never
    // said on the attention, with the relaunch at its full-power default.
    // (An exit whose last frame the server still reads as the agent's is
    // handed over only once it stops reading so; the last one here is.)
    let log = until(
        Duration::from_secs(10),
        "the exit journaled as the launch's own end",
        || harness_lines(&inst),
        |log| log.contains("exited; not relaunched"),
    );
    for step in log.lines().filter_map(|l| l.split_once("relaunch pid=")) {
        assert!(step.1.contains("step=ended:"), "{}", step.1);
    }
    assert_eq!(field(&status(&inst), "program"), Some("sh"));
    assert_eq!(attention(&inst), None, "nothing said");
}

/// Under the owner's `approve = "safe"` (full power, the default, answers
/// these boxes: aterm's `supervise_turn_end_live_headless.rs`).
#[test]
fn the_host_hands_over_what_it_cannot_prove_and_types_nothing() {
    let Some(inst) = boot("e", "approve = \"safe\"\n") else {
        return;
    };
    // The same breaker over /usr: escalated, naming the command.
    run_fake(&inst, "k1", &["bypass.hold", "rm-usr.txt"]);
    let text = escalated_and_quiet(&inst, "k1", "S=/usr; rm -rf $S/a");
    // Refused on WHERE it points — the scratch box of the first test was
    // answered under the same bypass start — not on the session's mode.
    assert!(
        text.contains("rm-breaker") && text.contains("scratch root"),
        "{text}"
    );
    // The human answers (this test's own key); the badge goes with the box.
    ctl_ok(&inst, &[&format!("@{}", inst.sid), "key", "z"]);
    keys_until(&inst, "k1", &["z"], Duration::from_secs(10));
    program_is(&inst, "sh", Duration::from_secs(10));
    until(
        Duration::from_secs(10),
        "the badge cleared",
        || attention(&inst).unwrap_or_default(),
        str::is_empty,
    );

    // A `touch x` box (not read-only): escalated, nothing typed…
    run_fake(&inst, "k2", &["touch.txt"]);
    escalated_and_quiet(&inst, "k2", "touch x");
    // …and switching the harness off in the file stops the supervisor at the
    // reload: its claim and its badge go, and still nothing is typed.
    let cfg = inst.tmp.join("cfg/aterm/aterm.toml");
    let text = std::fs::read_to_string(&cfg).expect("config");
    std::fs::write(&cfg, format!("{text}[harness]\nenabled = false\n"))
        .expect("rewrite the config");
    until(
        Duration::from_secs(20),
        "supervisor=- and no badge after enabled=false",
        || format!("{} attention={:?}", status(&inst), attention(&inst)),
        |s| field(s, "supervisor") == Some("-") && s.ends_with("attention=None"),
    );
    std::thread::sleep(QUIET);
    assert!(
        keys(&inst, "k2").is_empty(),
        "typed: {:?}",
        keys(&inst, "k2")
    );
}

/// THE SILENT LOOP, live (the 0.92.0 host's journal of 2026-09-24:
/// `ESCALATED seq=9246`, then nothing — its badge stood through the whole
/// workflow the owner's answer started, because the loop cleared it only at
/// the next idle point). Under `approve = "none"` (a limit the owner wrote:
/// every box is his) the owner's own box is escalated; a PERSON answers it
/// and the worker goes busy for ten seconds. The badge goes within 3 s of
/// the answer, while the worker is still busy. NEGATIVE CONTROL: before
/// the answer the badge stands and nothing is typed.
#[test]
fn a_box_a_person_answered_loses_its_badge_while_the_worker_works() {
    let Some(inst) = boot("b", "approve = \"none\"\n") else {
        return;
    };
    run_fake(&inst, "k1", &["owners-box.txt", "busy.hold", "busy.hold"]);
    escalated_and_quiet(&inst, "k1", "aterm-auto-target-x.noindex");
    // Both ends of the claim are the SERVER's own timeline records, read once
    // the clear has landed — as `the_host_answers_what_the_policy_proves_safe`
    // reads its attach. The client stopwatch this replaced ran from `ctl key`
    // returning to the last of a polling loop's `aterm ctl meta` spawns, so a
    // loaded gate's spawn latency failed it with the product right (the
    // load-sensitive test audit of 2026-09-27); the polls below only wait for
    // effects, and generously.
    let mark = timeline_since(&inst, 0).last().map_or(0, |e| e.0);
    // The person's answer (this test's own key: the fake reads it as one).
    ctl_ok(&inst, &[&format!("@{}", inst.sid), "key", "1"]);
    keys_until(&inst, "k1", &["1"], Duration::from_secs(15));
    until(
        Duration::from_secs(30),
        "the badge cleared",
        || attention(&inst).unwrap_or_default(),
        str::is_empty,
    );
    let events = timeline_since(&inst, mark);
    let busy = events
        .iter()
        .find(|e| e.2 == "agent-change" && e.3.starts_with("busy "))
        .unwrap_or_else(|| panic!("the answer never set the worker working: {events:?}"));
    let cleared = events
        .iter()
        .find(|e| e.2 == "meta-change" && e.3 == "field=attention value=-")
        .unwrap_or_else(|| panic!("the badge's clear is not on the timeline: {events:?}"));
    // THE REGRESSION: the badge stood until the worker's next idle point. So
    // the clear precedes (in the timeline's own record order) the first verdict
    // after `busy` that is not `busy`, whenever that has come, and lands within
    // 3 s of the worker going busy on the server's clock.
    if let Some(rested) = events
        .iter()
        .find(|e| e.2 == "agent-change" && e.0 > busy.0 && !e.3.starts_with("busy "))
    {
        assert!(
            cleared.0 < rested.0,
            "the badge cleared only when the worker stopped ({rested:?}): {events:?}"
        );
    }
    let late = cleared.1.saturating_sub(busy.1);
    assert!(
        late <= 3000,
        "the badge cleared {late} ms after the worker went busy: {events:?}"
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

#[test]
fn one_supervisor_per_session() {
    let Some(inst) = boot("o", "") else {
        return;
    };
    let sel = format!("@{}", inst.sid);
    // Hold the claim through launch and program detection. Those two control
    // calls can be slow under a full workspace gate; an 8-second claim started
    // here could lapse before the fake even begins, allowing the host to
    // answer and exit before `program_is` samples it.
    ctl_ok(
        &inst,
        &[
            &sel,
            "meta",
            "set",
            "supervisor",
            "someone-else",
            "ttl=60000",
        ],
    );
    run_fake(&inst, "k1", &["read.txt"]);
    program_is(&inst, "claude", Duration::from_secs(10));
    // Start the lease being tested only after Claude is observably running.
    ctl_ok(
        &inst,
        &[
            &sel,
            "meta",
            "set",
            "supervisor",
            "someone-else",
            "ttl=10000",
        ],
    );
    let claimed = Instant::now();
    // While it holds: the host stays off, nothing is typed.
    while claimed.elapsed() < Duration::from_secs(4) {
        let s = status(&inst);
        assert_eq!(field(&s, "supervisor"), Some("someone-else"), "{s}");
        assert!(keys(&inst, "k1").is_empty(), "typed under another's claim");
        std::thread::sleep(Duration::from_millis(100));
    }
    // It lapses: the host takes the session over and answers the box.
    keys_until(&inst, "k1", &["1"], Duration::from_secs(20));
    let s = status(&inst);
    assert!(
        field(&s, "supervisor").is_some_and(|v| v.starts_with("aterm-harness")),
        "{s}"
    );
}

/// NEGATIVE CONTROL: a headless instance whose `[harness]` says
/// `headless = false` runs no host — the box waits and nothing is typed.
#[test]
fn a_headless_instance_limited_by_its_config_supervises_nothing() {
    let Some(inst) = boot("n", "headless = false\n") else {
        return;
    };
    run_fake(&inst, "k1", &["read.txt"]);
    program_is(&inst, "claude", Duration::from_secs(10));
    let quiet_until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < quiet_until {
        let s = status(&inst);
        assert_eq!(field(&s, "supervisor"), Some("-"), "{s}");
        assert!(
            keys(&inst, "k1").is_empty(),
            "typed: {:?}",
            keys(&inst, "k1")
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------------------
// Relaunch on exit.
// ---------------------------------------------------------------------------

/// The conversation the relaunch fake registers when nothing resumes it.
const CONVERSATION: &str = "0badf00d-1111-2222-3333-444455556666";

/// Replace the world's fake with one that registers itself as Claude Code
/// does (`~/.claude/sessions/<pid>.json`: its conversation — the one it was
/// resumed on, else [`CONVERSATION`] — its kernel start as Claude renders it,
/// idle), logs its argv to `launches.log`, draws an idle composer, and dies
/// by SIGKILL once `die` exists (a crash: its record stays behind). It
/// answers `--version` as the managed twin is asked.
fn write_relaunch_fake(inst: &Instance) {
    use std::os::unix::fs::PermissionsExt;
    let tmp = inst.tmp.display();
    std::fs::write(inst.tmp.join("sc/idle.txt"), bypass_start()).expect("idle screen");
    let fake = format!(
        "#!/bin/sh\n\
         case \"$1\" in --version) echo '2.1.281 (Claude Code)'; exit 0 ;; esac\n\
         echo \"$$ $*\" >> {tmp}/launches.log\n\
         stty -icanon -echo min 1 2>/dev/null\n\
         sid={CONVERSATION}; prev=\n\
         for a in \"$@\"; do [ \"$prev\" = --resume ] && sid=$a; prev=$a; done\n\
         start=$(LC_ALL=C TZ=UTC ps -o lstart= -p $$ | awk '{{$1=$1; print}}')\n\
         mkdir -p \"$HOME/.claude/sessions\"\n\
         printf '{{\"pid\":%s,\"sessionId\":\"%s\",\"cwd\":\"%s\",\"version\":\"2.1.281\",\
         \"status\":\"idle\",\"statusUpdatedAt\":%s000,\"procStart\":\"%s\",\
         \"kind\":\"interactive\",\"entrypoint\":\"cli\"}}' \
         $$ \"$sid\" \"$PWD\" \"$(date +%s)\" \"$start\" > \"$HOME/.claude/sessions/$$.json\"\n\
         printf '\\033[2J\\033[H'\n\
         cat {tmp}/sc/idle.txt\n\
         while :; do\n\
         \x20 if [ -e {tmp}/die ]; then rm -f {tmp}/die; kill -KILL $$; fi\n\
         \x20 sleep 0.2\n\
         done\n"
    );
    let path = fake_path(&inst.tmp);
    std::fs::write(&path, fake).expect("write the fake");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// Each launch of the relaunch fake: `(pid, args)`.
fn launches(inst: &Instance) -> Vec<(u32, String)> {
    std::fs::read_to_string(inst.tmp.join("launches.log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (pid, args) = l.split_once(' ').unwrap_or((l, ""));
            Some((pid.parse().ok()?, args.to_string()))
        })
        .collect()
}

/// RELAUNCH ON EXIT, live: the tab's shell is bash (a job-control shell the
/// relaunch line is written for), the fake runs from it with a launch flag,
/// and dies by SIGKILL with no person at the tab. The host relaunches it in
/// the same tab — a NEW process, a child of the same shell, `--model opus`
/// kept and `--resume <its conversation>` appended — and the supervisor is
/// back on the session. NEGATIVE CONTROL: after `[harness] relaunch = false`
/// (a live reload), the next death is left alone: no launch, the tab's
/// shell holds it.
#[test]
fn an_agent_that_exits_unasked_is_relaunched_on_its_conversation() {
    let Some(inst) = boot("r", "") else {
        return;
    };
    write_relaunch_fake(&inst);
    let sel = format!("@{}", inst.sid);
    let tmp = inst.tmp.display();
    // A job-control shell in the tab the relaunch line is written for (the
    // isolation's /bin/sh is not one), as the tab's foreground job.
    ctl_ok(&inst, &[&sel, "send", "/bin/bash --norc --noprofile -i"]);
    ctl_ok(&inst, &[&sel, "key", "enter"]);
    program_is(&inst, "bash", Duration::from_secs(10));
    let line = format!(
        "cd {tmp}/work && '{}' --model opus",
        fake_path(&inst.tmp).display()
    );
    ctl_ok(&inst, &[&sel, "send", &line]);
    ctl_ok(&inst, &[&sel, "key", "enter"]);
    program_is(&inst, "claude", Duration::from_secs(10));
    until(
        Duration::from_secs(5),
        "the host attached",
        || status(&inst),
        |s| field(s, "supervisor").is_some_and(|v| v.starts_with("aterm-harness")),
    );
    // Let the worker read the agent it would relaunch, then crash it.
    std::thread::sleep(Duration::from_secs(2));
    let first = launches(&inst);
    assert_eq!(first.len(), 1, "{first:?}");
    std::fs::write(inst.tmp.join("die"), "").expect("crash");
    let deadline = Instant::now() + Duration::from_secs(60);
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
    program_is(&inst, "claude", Duration::from_secs(10));
    // The relaunch ends at adoption and the new agent is its supervisor's at
    // once; the continuation is OWED to the tab (its record in flight) until
    // that supervisor's loop first reaches an idle point, and TYPED there (the
    // record done, the resumed process named) — the engine hands its host the
    // idle point (`IdleHost::at_idle`) and the worker takes the carry-on in
    // the loop.
    until(
        Duration::from_secs(60),
        "adopted, and supervised again",
        || format!("{}\n{}", status(&inst), harness_lines(&inst)),
        |s| {
            s.lines()
                .any(|l| l.contains("relaunch pid=") && l.contains("step=adopted"))
                && field(s.lines().next().unwrap_or(""), "supervisor")
                    .is_some_and(|v| v.starts_with("aterm-harness"))
        },
    );
    let name = format!("{CONVERSATION}.json");
    let record = until(
        Duration::from_secs(60),
        "the continuation typed where the loop parked",
        || {
            ["state", "home"]
                .iter()
                .filter_map(|root| find_file(&inst.tmp.join(root), &name))
                .find(|p| p.parent().is_some_and(|d| d.ends_with("upgrade")))
                .and_then(|p| std::fs::read_to_string(p).ok())
                .unwrap_or_default()
        },
        |r| r.contains(r#""phase":"done""#) && !r.contains(r#""resumed_pid":0"#),
    );
    assert!(record.contains(r#""cause":"exit""#), "{record}");

    // NEGATIVE CONTROL: the owner limits it; the next death is left alone.
    let cfg = inst.tmp.join("cfg/aterm/aterm.toml");
    let text = std::fs::read_to_string(&cfg).expect("config");
    std::fs::write(
        &cfg,
        text.replace("[harness]\n", "[harness]\nrelaunch = false\n"),
    )
    .expect("rewrite the config");
    std::thread::sleep(Duration::from_secs(3));
    std::fs::write(inst.tmp.join("die"), "").expect("crash again");
    program_is(&inst, "bash", Duration::from_secs(10));
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(launches(&inst).len(), 2, "relaunch = false: left alone");
    assert_eq!(field(&status(&inst), "program"), Some("bash"));
}

// ---------------------------------------------------------------------------
// The live upgrade, a step of the worker.
// ---------------------------------------------------------------------------

/// Put the upgrade fake at the managed twin's path, reporting `version` —
/// replaced by a rename, as atpkg re-renders the twin, so a running copy
/// keeps the file it was started from. It registers itself as Claude does
/// (its status settled a minute ago), draws an idle composer, and reads each
/// line typed into it: an announcement's READY marker is answered in its
/// transcript, the carry-on with a turn of the running build's own. SIGTERM
/// ends it as Claude's graceful shutdown does, its record removed.
fn write_upgrade_fake(inst: &Instance, version: &str) {
    use std::os::unix::fs::PermissionsExt;
    let tmp = inst.tmp.display();
    std::fs::write(inst.tmp.join("sc/idle.txt"), bypass_start()).expect("idle screen");
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
         case \"$1\" in --version) echo \"$VERSION (Claude Code)\"; exit 0 ;; esac\n\
         echo \"$$ $VERSION $*\" >> {tmp}/launches.log\n\
         sid={CONVERSATION}; prev=\n\
         for a in \"$@\"; do [ \"$prev\" = --resume ] && sid=$a; prev=$a; done\n\
         start=$(LC_ALL=C TZ=UTC ps -o lstart= -p $$ | awk '{{$1=$1; print}}')\n\
         mkdir -p \"$HOME/.claude/sessions\" \"$HOME/.claude/projects/-w\"\n\
         T=\"$HOME/.claude/projects/-w/$sid.jsonl\"\n\
         printf '{{\"pid\":%s,\"sessionId\":\"%s\",\"cwd\":\"%s\",\"version\":\"%s\",\
         \"status\":\"idle\",\"statusUpdatedAt\":%s000,\"procStart\":\"%s\",\
         \"kind\":\"interactive\",\"entrypoint\":\"cli\"}}' \
         $$ \"$sid\" \"$PWD\" \"$VERSION\" \"$(( $(date +%s) - 60 ))\" \"$start\" \
         > \"$HOME/.claude/sessions/$$.json\"\n\
         trap 'rm -f \"$HOME/.claude/sessions/$$.json\"; exit 0' TERM\n\
         stty -echo 2>/dev/null\n\
         printf '\\033[2J\\033[H'\n\
         cat {tmp}/sc/idle.txt\n\
         while IFS= read -r line; do\n\
         \x20 m=$(printf '%s' \"$line\" | grep -o 'ATERM-UPGRADE-READY-[0-9a-f]*' | head -1)\n\
         \x20 [ -n \"$m\" ] && {ready}\n\
         \x20 case \"$line\" in *Upgraded*) {carry} ;; esac\n\
         done\n",
        ready = row("\"$m\""),
        carry = row("'carrying on'"),
    );
    let path = fake_path(&inst.tmp);
    let next = path.with_extension("next");
    std::fs::write(&next, fake).expect("write the fake");
    std::fs::set_permissions(&next, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    std::fs::rename(&next, &path).expect("replace the twin");
}

/// THE LIVE UPGRADE AS A STEP OF THE WORKER, live: a fake Claude Code at
/// 2.1.281 runs as bash's foreground job; the managed twin is replaced by
/// 2.1.282 and atpkg's activation notice is written (a push — no sweep runs).
/// The worker takes its loop's idle point, announces, reads the READY
/// answer at the next one, ends the agent with SIGTERM and relaunches it in
/// the same tab ON THE NEW BUILD, on its conversation (`--resume <id>`, its
/// flags kept), and tells it to carry on. NEGATIVE CONTROL: before the
/// notice, a session on the current build is never announced to.
#[cfg(target_os = "macos")] // atpkg::activation_notice is macOS-only
#[test]
fn an_activation_notice_upgrades_the_session_at_its_idle_point() {
    let Some(inst) = boot("u", "relaunch = false\n") else {
        return;
    };
    write_upgrade_fake(&inst, "2.1.281");
    let sel = format!("@{}", inst.sid);
    let tmp = inst.tmp.display();
    ctl_ok(&inst, &[&sel, "send", "/bin/bash --norc --noprofile -i"]);
    ctl_ok(&inst, &[&sel, "key", "enter"]);
    program_is(&inst, "bash", Duration::from_secs(10));
    let line = format!(
        "cd {tmp}/work && '{}' --model opus",
        fake_path(&inst.tmp).display()
    );
    ctl_ok(&inst, &[&sel, "send", &line]);
    ctl_ok(&inst, &[&sel, "key", "enter"]);
    program_is(&inst, "claude", Duration::from_secs(10));
    until(
        Duration::from_secs(5),
        "the host attached",
        || status(&inst),
        |s| field(s, "supervisor").is_some_and(|v| v.starts_with("aterm-harness")),
    );
    // NEGATIVE CONTROL: current, so nothing is announced.
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !harness_lines(&inst).contains("announced"),
        "{}",
        harness_lines(&inst)
    );
    // A newer build lands, and atpkg says so.
    write_upgrade_fake(&inst, "2.1.282");
    let prefix = atpkg::store::default_prefix(&inst.tmp.join("home"));
    let notices = prefix.join(atpkg::activation_notice::NOTICE_DIR);
    std::fs::create_dir_all(&notices).expect("notice dir");
    std::fs::write(notices.join(".m.tmp"), "1\n").expect("marker");
    std::fs::rename(
        notices.join(".m.tmp"),
        notices.join(atpkg::activation_notice::CLAUDE_MARKER),
    )
    .expect("the activation notice");
    let deadline = Instant::now() + Duration::from_secs(150);
    let got = loop {
        let got = launches(&inst);
        if got.len() >= 2 || Instant::now() >= deadline {
            break got;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    assert_eq!(
        got.len(),
        2,
        "relaunched once: {got:?}\nharness log:\n{}",
        harness_lines(&inst)
    );
    assert_ne!(got[0].0, got[1].0, "a new process");
    assert_eq!(
        got[1].1,
        format!("2.1.282 --model opus --resume {CONVERSATION}"),
        "the new build, its flags kept, its conversation resumed"
    );
    let log = until(
        Duration::from_secs(60),
        "announced, then told to carry on",
        || harness_lines(&inst),
        |l| {
            l.contains("step=announced:1")
                && (l.contains("step=done") || l.contains("step=continued"))
        },
    );
    assert!(!log.contains("minute"), "{log}");
    program_is(&inst, "claude", Duration::from_secs(10));
}
