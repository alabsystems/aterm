// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless conformance for the IN-GUI SUPERVISOR HOST (`aterm-gui`'s
//! `harness_host`): a headless instance whose `aterm.toml` says `[harness]
//! headless = true` supervises every Claude Code session it owns with nothing
//! started by hand — no `aterm supervise`, no command typed for it.
//!
//! The worker is a FAKE `claude`: a `#!/bin/sh` script named `claude` (so the
//! server publishes `program=claude`, the shim rule of `session_program`)
//! that draws aterm-phase's measured Claude Code 2.1.280 screens one after
//! another, waits for ONE key after each and appends it to a key log; a
//! screen named `*.hold` is shown for a few seconds and reads nothing. The
//! key log is the ground truth of what the host typed.
//!
//! * THE HOST ATTACHES: `status supervisor=` names it within 2 s of
//!   `program=claude`.
//! * WHAT THE POLICY PROVES SAFE IS ANSWERED: a read-only Bash box gets `1`;
//!   the session survey `0`; the rm circuit breaker in a bypass session over
//!   a scratch path `1`.
//! * WHAT IT CANNOT PROVE IS HANDED TO THE HUMAN: the same breaker over
//!   `S=/usr` and a `touch x` box leave the attention set, naming the
//!   command, and nothing typed; switching `[harness] enabled` off in the file
//!   stops the supervisor (its claim and its badge go) and still types nothing.
//! * ONE SUPERVISOR PER SESSION: another holder's live claim keeps the host
//!   off the session; it attaches once that claim lapses.
//! * NEGATIVE CONTROL: the same headless instance without `headless = true`
//!   runs no host — the box waits, `supervisor=-`, nothing typed.
//!
//! ISOLATION: scratch HOME/XDG roots, a private control socket, a config with
//! every automatic lane off, `--no-reroute`, `SHELL=/bin/sh`
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

/// Boot one headless instance, 40x200, its `[harness]` table `harness`
/// appended to the isolation config. `None` = an environmental refusal,
/// announced as a SKIP with the log tail.
fn boot(tag: &str, harness: &str) -> Option<Instance> {
    let Some(tmp) = scratch_root(tag) else {
        eprintln!("SKIP: no scratch base with a short enough socket path");
        return None;
    };
    let cfg = tmp.join("cfg/aterm/aterm.toml");
    let base = std::fs::read_to_string(&cfg).expect("the isolation config");
    std::fs::write(&cfg, format!("{base}{harness}")).expect("write the config");
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
        // The supervisor's lines (`harness @<sid>: …`) are the diagnosis a
        // failed assertion prints ([`harness_lines`]).
        .env("ATERM_LOG", "info")
        .env("ATERM_LINES", "40")
        // The fixtures are 120-140 columns wide; Claude Code lays out its own
        // rows, so a terminal wrap is not a shape it draws.
        .env("ATERM_COLUMNS", "200")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
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
        if is_socket_or_symlink(&sock_path) {
            inst.sid = boot_session(&inst);
            return Some(inst);
        }
        std::thread::sleep(POLL_GAP);
    }
    eprintln!(
        "SKIP: control socket never appeared; log tail:\n{}",
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
         # each into the key log ($1); a *.hold screen is shown and nothing read.\n\
         log=$1; shift\n\
         old=$(stty -g)\n\
         stty -icanon -echo min 1\n\
         for scene in \"$@\"; do\n\
         \x20 printf '\\033[2J\\033[H'\n\
         \x20 cat \"$scene\"\n\
         \x20 case \"$scene\" in *.hold) sleep {HOLD_S}; continue ;; esac\n\
         \x20 k=$(dd bs=1 count=1 2>/dev/null | od -An -c | tr -d ' ')\n\
         \x20 printf '%s\\n' \"$k\" >> \"$log\"\n\
         done\n\
         stty \"$old\"\n\
         printf '\\033[2J\\033[H'\n"
    );
    std::fs::write(&claude, fake).expect("write the fake");
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    // The measured touch box draws its third option garbled (`3. Nooject`,
    // aterm-phase's `the_touch_box_is_bash_whatever_its_description_says`),
    // and the engine presses a one-shot `Yes` only on a box that also offers
    // a readable `No` (`once_choice`): the read box repairs that row, so what
    // it tests is the read-only rule, not the garble. (`touch.txt` keeps it:
    // that box is escalated either way.)
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
        ("survey.txt", survey),
        ("busy.hold", body(SPINNER)),
        ("bypass.hold", bypass_start()),
        ("rm-scratch.txt", rm_box("S=/tmp/athh-scratch; rm -rf $S/a")),
        ("rm-usr.txt", rm_box("S=/usr; rm -rf $S/a")),
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

/// The keys the fake read, one per line.
fn keys(inst: &Instance, log: &str) -> Vec<String> {
    std::fs::read_to_string(inst.tmp.join(log))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
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

const HEADLESS_ON: &str = "[harness]\nheadless = true\n";

#[test]
fn the_host_answers_what_the_policy_proves_safe() {
    let Some(inst) = boot("a", HEADLESS_ON) else {
        return;
    };
    // A read-only Bash box: the host attaches within 2 s of `program=claude`
    // and presses `1`.
    run_fake(&inst, "k1", &["read.txt"]);
    program_is(&inst, "claude", Duration::from_secs(10));
    let seen = Instant::now();
    let rec = until(
        Duration::from_secs(2),
        "status supervisor= names the host",
        || status(&inst),
        |s| field(s, "supervisor").is_some_and(|v| v.starts_with("aterm-harness")),
    );
    assert!(seen.elapsed() <= Duration::from_secs(2), "{rec}");
    keys_until(&inst, "k1", &["1"], Duration::from_secs(15));
    program_is(&inst, "sh", Duration::from_secs(10));

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
}

#[test]
fn the_host_hands_over_what_it_cannot_prove_and_types_nothing() {
    let Some(inst) = boot("e", HEADLESS_ON) else {
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
    std::fs::write(
        &cfg,
        text.replace(HEADLESS_ON, "[harness]\nheadless = true\nenabled = false\n"),
    )
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

#[test]
fn one_supervisor_per_session() {
    let Some(inst) = boot("o", HEADLESS_ON) else {
        return;
    };
    let sel = format!("@{}", inst.sid);
    // Another supervisor holds the session (a lease that lapses in 8 s).
    ctl_ok(
        &inst,
        &[
            &sel,
            "meta",
            "set",
            "supervisor",
            "someone-else",
            "ttl=8000",
        ],
    );
    let claimed = Instant::now();
    run_fake(&inst, "k1", &["read.txt"]);
    program_is(&inst, "claude", Duration::from_secs(10));
    // While it holds: the host stays off, nothing is typed.
    while claimed.elapsed() < Duration::from_secs(6) {
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

/// NEGATIVE CONTROL: a headless instance whose `[harness]` does not say
/// `headless = true` runs no host — the box waits and nothing is typed.
#[test]
fn a_headless_instance_supervises_nothing_by_default() {
    let Some(inst) = boot("n", "") else {
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
