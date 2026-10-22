// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `muse-captures [--only <regex of names>]` (about ten minutes): Muse Code's
//! screens on THIS build, each group on a lab of its own (the harness off),
//! into `$LAB_STATE/captures/muse-<version>-<name>.txt` (+ png), in the repo's
//! fixture shape; beside a box's capture the stub's log of its turn
//! (`<name>.stub.log`: the scripted call and the result that came back), and
//! `muse --help` run in the jail (`cli-help.txt`). Run it on a new Muse Code
//! build before anything presses a key on it.
//!
//! The groups: the workspace-trust box (a fresh scratch HOME, no
//! `--trust-workspace`); idle and a draft; the slash menu; a turn's end, a
//! turn running and an interrupt; the exit (one ctrl+c, then the second); the
//! shell box (`bash` with `sandbox_permissions: require_escalated`, the one
//! shell call that asks) approved and denied; the file-write box (a protected
//! path, `.agents/`) approved; the edit box on the same file denied; the
//! question (`request_user_input`), then Down and Enter, and Down twice onto
//! `None of the above` and Enter; a turn ended on each tool's own rows with
//! no words after them ([`TOOL_ROWS`]: a read, a search, a write, an edit,
//! a shell call, the memory, the todos, the goal and the work list); the
//! 429 with its retry row and its final
//! error (`provider_retry.max_attempts=3`, the stub's `retry-after: 5`); the
//! 401; the 500 with its retry row and its final error; the usage wall (a
//! 429 `usage_limit_reached` with `resets_at`, the stub's `limit:` mode: Muse
//! Code draws `Usage limit reached · /upgrade (<url>) …` from it); `/usage`
//! and `/status` after one turn.
//!
//! Nothing is typed into a box but the key its capture names, and `/usage`
//! and `/status` are entered only on positive evidence that the menu offers
//! that very command on its first row (Enter runs the row the menu marks).
//! Every step a capture's header names must have happened: a turn not taken,
//! a box not drawn ends the run with no capture written on it.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use crate::LAB;
use crate::door::Door;
use crate::lab::UpOpts;
use crate::paths::paths;
use crate::procs::JailOut;
use crate::run::Run;
use crate::util::scratch_text;
use crate::{Fail, Res, stop};

/// An ordinary turn's prompt: Muse Code answers a bare greeting itself, with no request.
const PROMPT: &str = "carry on with the task";

/// The stub's reply to a turn in mode `ok` (`… (reply N)`), drawn.
const REPLIED: &str = r"\(reply [0-9]+\)";

/// Muse Code's status row while a turn runs (`◆ Thinking (2s · esc to interrupt)`).
const BUSY: &str = "^[◇◈◆] .*esc to interrupt";

/// The stub's answer once a scripted call's result came back.
const DONE: &str = "Tool call done";

/// The shell call that asks: `bash` leaving the sandbox.
const SHELL: &str = r#"{"name":"bash","input":{"command":"touch probe.txt","description":"Create the probe file","sandbox_permissions":"require_escalated"}}"#;

/// A write to a protected path (`.agents/`), which asks.
const WRITE: &str = r#"{"name":"write_file","input":{"path":".agents/probe-write.md","content":"written by the lab stub\n"}}"#;

/// An edit of the same protected file.
const EDIT: &str = r#"{"name":"edit_file","input":{"path":".agents/probe-write.md","find":"written","replace":"edited"}}"#;

/// One single-select question with two options (Muse Code adds `None of the above`).
const QUESTION: &str = r#"{"name":"request_user_input","input":{"questions":[{"id":"color","header":"Color","question":"Which color should the probe use?","options":[{"label":"Red (Recommended)","description":"Warm and loud."},{"label":"Blue","description":"Cool and calm."}]}]}}"#;

/// THE TOOL ROWS (`tool-row-<name>`): each call Muse Code's tool script can
/// make in the lab without a box or the network, answered once, its result
/// answered with a reply holding NO WORDS, so the turn ends on the call's own
/// rows above the composer (as `forever --mode resid`'s `tool-end` chain ends
/// on `  Read README.md`). `(capture name, tool, its input)`; `@` in the
/// input is the project's `README.md` text the lab put there first.
pub const TOOL_ROWS: [(&str, &str, &str); 12] = [
    ("read", "read_file", r#"{"path":"README.md"}"#),
    (
        "search",
        "search",
        r#"{"pattern":"parser","paths":["README.md"]}"#,
    ),
    (
        "search-none",
        "search",
        r#"{"pattern":"no-such-word-here"}"#,
    ),
    (
        "write",
        "write_file",
        r#"{"path":"notes.txt","content":"written by the lab stub\n"}"#,
    ),
    (
        "edit",
        "edit_file",
        r#"{"path":"README.md","find":"parser","replace":"lexer"}"#,
    ),
    ("bash", "bash", r#"{"command":"ls"}"#),
    (
        "bash-described",
        "bash",
        r#"{"command":"wc -l README.md","description":"Count the README's lines"}"#,
    ),
    ("read-memory", "read_memory", r#"{"path":"notes.md"}"#),
    (
        "add-memory",
        "add_memory",
        r#"{"path":"notes.md","content":"the lab's note"}"#,
    ),
    (
        "todos",
        "write_todos",
        r#"{"todos":[{"text":"write the parser","status":"in_progress"},{"text":"land it","status":"pending"}]}"#,
    ),
    ("goal", "get_goal", "{}"),
    ("work-list", "work_list", "{}"),
];

/// The README the tool rows' project holds (one line naming the parser).
const README: &str = "the lab's project: a parser module";

/// The captures' own lab.
struct Cap {
    tag: String,
    ver: String,
    dir: PathBuf,
}

impl Cap {
    /// A lab of its own (the harness off), the previous one down.
    fn fresh(&self) -> Result<Run, Fail> {
        let _ = crate::lab::down(&self.tag, false);
        let o = UpOpts {
            harness_keys: "enabled = false".into(),
            lifeline: true,
            ..UpOpts::from_env()
        };
        crate::lab::up(&self.tag, &o).map_err(|f| {
            if f.stopped.is_some() {
                f
            } else {
                Fail::die(format!(
                    "muse-captures: up failed: {}",
                    f.msg.unwrap_or_default()
                ))
            }
        })
    }

    /// Muse Code launched in `run` with `args`, and the lab as it is then,
    /// once `drawn` shows.
    fn muse(&self, run: &Run, args: &[&str], drawn: &str) -> Result<Run, Fail> {
        let l = crate::muse::Launch {
            args: args.iter().map(|s| (*s).to_string()).collect(),
            ..crate::muse::Launch::default()
        };
        crate::muse::launch(run, &l).map_err(|f| {
            Fail::die(format!(
                "muse-captures: muse did not start: {}",
                f.msg.unwrap_or_default()
            ))
        })?;
        let run = Run::load(&self.tag)?;
        self.wait(&run, drawn, 60)?;
        Ok(run)
    }

    /// [`Cap::fresh`], then Muse Code trusting the workspace (`--trust-workspace`
    /// and `extra`), at its composer.
    fn fresh_muse(&self, extra: &[&str]) -> Result<Run, Fail> {
        let run = self.fresh()?;
        let mut args = vec!["--trust-workspace"];
        args.extend_from_slice(extra);
        self.muse(&run, &args, "^❯")
    }

    fn must(&self, ok: bool, what: &str) -> Result<(), Fail> {
        if ok {
            Ok(())
        } else {
            Err(Fail::die(format!(
                "muse-captures: {what} did not happen, so nothing is captured on it ({LAB} text {})",
                self.tag
            )))
        }
    }

    /// `ctl <args>` on the lab, which must succeed.
    fn ctl(&self, run: &Run, args: &[&str]) -> Result<(), Fail> {
        let ok = Door::new(run)?.ctl(args)?.code == 0;
        self.must(ok, &format!("ctl {}", args.join(" ")))
    }

    fn key(&self, run: &Run, key: &str) -> Result<(), Fail> {
        self.ctl(run, &["key", key])
    }

    fn turn(&self, run: &Run, text: &str) -> Result<(), Fail> {
        let taken = crate::verbs::turn(&Door::new(run)?, text)?;
        self.must(taken, &format!("turn {text}"))
    }

    fn wait(&self, run: &Run, re: &str, secs: u64) -> Result<(), Fail> {
        let seen = Door::new(run)?.seen(re, secs)?;
        self.must(seen, &format!("wait '{re}'"))
    }

    /// After a reply, until the run behind it is over: Muse Code's reminder
    /// agents make their side calls once the reply is drawn (measured: a
    /// ctrl+c a moment after the reply drew `◆ Interrupting (0s · esc to
    /// interrupt)`, not the exit prompt), and its tip side call follows about
    /// four seconds later. Five seconds, then a screen quiet for 1.5 s.
    fn settle(&self, run: &Run) -> Result<(), Fail> {
        stop::sleep(Duration::from_secs(5))?;
        let quiet = Door::new(run)?.await_idle(1500, 20)?;
        self.must(quiet, "the screen settling after the reply")
    }

    fn cap(&self, run: &Run, name: &str, what: &str) -> Result<(), Fail> {
        crate::verbs::capture(run, name, what)?;
        Ok(())
    }

    fn mode(&self, run: &Run, m: &str) -> Result<(), Fail> {
        crate::verbs::set_mode(run, m).map(drop)
    }

    /// The stub's `tool` mode with `script`.
    fn tool(&self, run: &Run, script: &str) -> Result<(), Fail> {
        crate::util::atomic_write(&run.w.join("tool.json"), script.as_bytes())
            .map_err(|e| Fail::die(format!("cannot write the tool script: {e}")))?;
        self.mode(run, "tool")
    }

    /// The stub's log of the lab's turns (its `POST /responses` lines but
    /// the side calls) beside the capture `name`.
    fn stub_log(&self, run: &Run, name: &str) {
        let mut text = format!(
            "# muse {} · the stub's log of the turns behind muse-{}-{name} (the lab's fixed fake key is logged as auth=key, never a value)\n",
            self.ver, self.ver
        );
        for l in scratch_text(&run.w.join("stub.log"))
            .lines()
            .filter(|l| l.contains("POST /responses") && !l.contains("kind=muse-side"))
        {
            text.push_str(&l.chars().take(400).collect::<String>());
            text.push('\n');
        }
        let f = self.dir.join(format!("muse-{}-{name}.stub.log", self.ver));
        match fs::write(&f, text) {
            Ok(()) => println!("{}", f.display()),
            Err(e) => eprintln!(
                "aterm-lab: muse-captures: cannot write {}: {e}",
                f.display()
            ),
        }
    }

    /// A file in the project dir, made INSIDE the jail (the tab's shell could
    /// have put a link at that path): `printf '%s\n' TEXT > proj/<rel>`.
    fn project_file(&self, run: &Run, rel: &str, text: &str) -> Result<(), Fail> {
        let (rc, _) = crate::procs::jailed(
            run,
            std::path::Path::new(&run.proj),
            &[],
            &[
                "/bin/sh",
                "-c",
                "mkdir -p \"$(dirname \"$1\")\" && printf '%s\\n' \"$2\" > \"$1\"",
                "sh",
                rel,
                text,
            ],
            JailOut::Capture,
        )
        .map_err(Fail::die)?;
        self.must(rc == 0, &format!("the project file {rel}"))
    }
}

/// `muse-captures [--only <regex of names>]`.
pub fn cmd_muse_captures(args: &[String]) -> Res {
    let only = match args.first().map(String::as_str) {
        Some("--only") => args
            .get(1)
            .cloned()
            .ok_or_else(|| Fail::die("muse-captures: --only needs a pattern"))?,
        Some(other) => {
            return Err(Fail::die(format!(
                "muse-captures: unknown option '{other}'"
            )));
        }
        None => ".".into(),
    };
    let re =
        aterm_regex::Regex::new(&only).map_err(|e| Fail::die(format!("muse-captures: {e}")))?;
    stop::install();
    let dir = paths().state.join("captures");
    fs::create_dir_all(&dir)
        .map_err(|e| Fail::die(format!("cannot make {}: {e}", dir.display())))?;
    let mut c = Cap {
        tag: "muse-captures".into(),
        ver: String::new(),
        dir,
    };
    let result = captures(&mut c, &|name: &str| re.is_match(name));
    let _ = crate::lab::down(&c.tag, false);
    result?;
    let n = fs::read_dir(&c.dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .starts_with(&format!("muse-{}-", c.ver))
                })
                .count()
        })
        .unwrap_or(0);
    println!("muse-captures: done ({n} files)");
    Ok(0)
}

/// Whether the screen offers `cmd` on the slash menu's first row with the
/// composer reading exactly `cmd`: the evidence Enter needs to run that
/// command and no other.
#[must_use]
pub fn menu_offers_first(screen: &str, cmd: &str) -> bool {
    let rows: Vec<String> = screen.lines().map(str::to_string).collect();
    let Some((caret, lines)) = aterm_phase::muse::composer_draft(&rows) else {
        return false;
    };
    if lines.first().map(String::as_str) != Some(cmd) {
        return false;
    }
    rows.get(caret + 1).is_some_and(|r| {
        r.trim_start()
            .strip_prefix(cmd)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    })
}

#[allow(clippy::too_many_lines)]
fn captures(c: &mut Cap, want: &dyn Fn(&str) -> bool) -> Result<(), Fail> {
    let any = |names: &[&str]| names.iter().any(|n| want(n));
    let run = c.fresh()?;
    crate::muse::need_muse(&run)?;
    c.ver.clone_from(&run.muse_ver);
    println!(
        "muse-captures: Muse Code {} -> {}/muse-{}-*",
        c.ver,
        c.dir.display(),
        c.ver
    );
    if want("cli-help") {
        let home = run.w.join("home");
        let home = home.to_string_lossy();
        let (rc, out) = crate::procs::jailed(
            &run,
            &run.w,
            &[("HOME", &home)],
            &[&run.muse_bin, "--help"],
            JailOut::CaptureAll,
        )
        .map_err(Fail::die)?;
        let f = c.dir.join(format!("muse-{}-cli-help.txt", c.ver));
        fs::write(
            &f,
            format!(
                "# muse {} · muse --help, in the lab's jail with a scratch HOME (exit {rc})\n{out}",
                c.ver
            ),
        )
        .map_err(|e| Fail::die(e.to_string()))?;
        println!("{}", f.display());
    }
    // ---- the workspace-trust box: a fresh scratch HOME has trusted no folder
    if want("box-trust") {
        let run = c.muse(&run, &[], "Do you trust this workspace")?;
        stop::sleep(Duration::from_millis(800))?;
        c.cap(
            &run,
            "box-trust",
            "launched in the lab's project dir with a fresh scratch HOME and no --trust-workspace: the workspace-trust box, before any key",
        )?;
    }
    // ---- idle, a draft
    if any(&["idle", "draft"]) {
        let run = c.fresh_muse(&[])?;
        stop::sleep(Duration::from_secs(1))?;
        c.cap(
            &run,
            "idle",
            "launched with --trust-workspace: idle at its composer, no turn yet",
        )?;
        c.ctl(&run, &["send", "a draft, not sent"])?;
        stop::sleep(Duration::from_millis(1500))?;
        c.cap(&run, "draft", "`a draft, not sent` typed, no Enter")?;
    }
    if want("slash-menu") {
        let run = c.fresh_muse(&[])?;
        c.ctl(&run, &["send", "/"])?;
        stop::sleep(Duration::from_millis(1500))?;
        c.cap(
            &run,
            "slash-menu",
            "`/` typed into the composer: the command menu",
        )?;
    }
    // ---- a turn's end, a turn running, an interrupt
    if any(&[
        "end-of-turn",
        "busy-thinking",
        "interrupted",
        "interrupted-settled",
    ]) {
        let run = c.fresh_muse(&[])?;
        c.turn(&run, PROMPT)?;
        c.wait(&run, REPLIED, 30)?;
        stop::sleep(Duration::from_secs(1))?;
        c.cap(
            &run,
            "end-of-turn",
            &format!("`{PROMPT}` and Enter, the stub's `ok` reply drawn: the end of a turn"),
        )?;
        c.mode(&run, "slow")?;
        c.turn(&run, "a slow turn")?;
        c.wait(&run, BUSY, 30)?;
        stop::sleep(Duration::from_secs(3))?;
        c.cap(
            &run,
            "busy-thinking",
            "after one ended turn, `a slow turn` in the stub's `slow` mode (one `tick N` a second for 30 s, no text shown until the item ends): the turn running, 3 s after its status row showed",
        )?;
        c.key(&run, "escape")?;
        stop::sleep(Duration::from_secs(2))?;
        c.cap(
            &run,
            "interrupted",
            "the same running turn after one Esc, 2 s later",
        )?;
        let settled = Door::new(&run)?.await_gone("interrupting run", 20)?;
        c.must(settled, "the interrupt settling")?;
        stop::sleep(Duration::from_secs(1))?;
        c.cap(
            &run,
            "interrupted-settled",
            "the same interrupted turn once its footer no longer says `interrupting run`",
        )?;
    }
    // ---- the exit
    if any(&["exit-confirm", "exited-resume"]) {
        let run = c.fresh_muse(&[])?;
        c.turn(&run, PROMPT)?;
        c.wait(&run, REPLIED, 30)?;
        c.settle(&run)?;
        c.key(&run, "ctrl+c")?;
        c.wait(&run, "Ctrl-C again", 10)?;
        c.cap(
            &run,
            "exit-confirm",
            "after one ended turn (its run settled: 5 s and a quiet screen), one ctrl+c at the empty composer",
        )?;
        c.key(&run, "ctrl+c")?;
        c.wait(&run, "To continue this session", 15)?;
        stop::sleep(Duration::from_secs(1))?;
        c.cap(
            &run,
            "exited-resume",
            "the second ctrl+c: Muse Code has exited to the shell",
        )?;
    }
    // ---- the shell box
    for (name, key, what) in [
        (
            "box-shell-approved",
            "enter",
            "Enter on its focused first option",
        ),
        ("box-shell-denied", "escape", "Esc"),
    ] {
        if !any(&["box-shell", name]) {
            continue;
        }
        let run = c.fresh_muse(&[])?;
        c.tool(&run, SHELL)?;
        c.turn(&run, "toolstart")?;
        c.wait(&run, "Would you like to run the following command", 30)?;
        stop::sleep(Duration::from_secs(1))?;
        if name == "box-shell-approved" {
            c.cap(&run, "box-shell", "the stub's call `muse.bash {command: \"touch probe.txt\", description: \"Create the probe file\", sandbox_permissions: \"require_escalated\"}` (the one shell call that asks in the default on-request mode): the shell approval box, before any key")?;
        }
        c.key(&run, key)?;
        c.wait(&run, DONE, 30)?;
        stop::sleep(Duration::from_secs(1))?;
        c.cap(
            &run,
            name,
            &format!("the shell approval box answered with {what}: the stub's answer quotes the tool result Muse Code sent back"),
        )?;
        c.stub_log(&run, name);
    }
    // ---- the file-write box (a protected path), approved
    if any(&["box-write", "box-write-approved"]) {
        let run = c.fresh_muse(&[])?;
        c.tool(&run, WRITE)?;
        c.turn(&run, "toolstart")?;
        c.wait(&run, "Would you like to allow this file access", 30)?;
        stop::sleep(Duration::from_secs(1))?;
        c.cap(&run, "box-write", "the stub's call `muse.write_file {path: \".agents/probe-write.md\", content}` (a protected path: a write in the workspace elsewhere does not ask): the file-access box, before any key")?;
        c.key(&run, "enter")?;
        c.wait(&run, DONE, 30)?;
        stop::sleep(Duration::from_secs(1))?;
        c.cap(
            &run,
            "box-write-approved",
            "the file-access box of the write answered with Enter on its focused first option",
        )?;
        c.stub_log(&run, "box-write-approved");
    }
    // ---- the edit box on the same file, denied
    if any(&["box-edit", "box-edit-denied"]) {
        let run = c.fresh()?;
        c.project_file(&run, ".agents/probe-write.md", "written by the lab")?;
        let run = c.muse(&run, &["--trust-workspace"], "^❯")?;
        c.tool(&run, EDIT)?;
        c.turn(&run, "toolstart")?;
        c.wait(&run, "Would you like to allow this file access", 30)?;
        stop::sleep(Duration::from_secs(1))?;
        c.cap(&run, "box-edit", "the stub's call `muse.edit_file {path: \".agents/probe-write.md\", find: \"written\", replace: \"edited\"}` on a file that holds `written by the lab` (a protected path): the file-access box, before any key")?;
        c.key(&run, "escape")?;
        c.wait(&run, DONE, 30)?;
        stop::sleep(Duration::from_secs(1))?;
        c.cap(
            &run,
            "box-edit-denied",
            "the file-access box of the edit answered with Esc",
        )?;
        c.stub_log(&run, "box-edit-denied");
    }
    // ---- the question: Down and Enter
    if any(&["question", "question-down", "question-answered"]) {
        let run = c.fresh_muse(&[])?;
        c.tool(&run, QUESTION)?;
        c.turn(&run, "toolstart")?;
        c.wait(&run, "Which color should the probe use", 30)?;
        stop::sleep(Duration::from_secs(1))?;
        c.cap(&run, "question", "the stub's call `muse.request_user_input {questions: [{id: color, header: Color, question: \"Which color should the probe use?\", options: [Red (Recommended), Blue]}]}`: the question box, before any key")?;
        c.key(&run, "down")?;
        stop::sleep(Duration::from_secs(1))?;
        c.cap(&run, "question-down", "the question box after one Down")?;
        c.key(&run, "enter")?;
        c.wait(&run, DONE, 30)?;
        stop::sleep(Duration::from_secs(1))?;
        c.cap(&run, "question-answered", "the question box after one Down and Enter: the stub's answer quotes the answer Muse Code sent back")?;
        c.stub_log(&run, "question-answered");
    }
    // ---- the question: `None of the above` and Enter
    if any(&["question-none", "question-none-enter"]) {
        let run = c.fresh_muse(&[])?;
        c.tool(&run, QUESTION)?;
        c.turn(&run, "toolstart")?;
        c.wait(&run, "Which color should the probe use", 30)?;
        stop::sleep(Duration::from_secs(1))?;
        c.key(&run, "down")?;
        stop::sleep(Duration::from_millis(500))?;
        c.key(&run, "down")?;
        stop::sleep(Duration::from_secs(1))?;
        c.cap(
            &run,
            "question-none",
            "the question box after two Downs (onto its third option)",
        )?;
        c.key(&run, "enter")?;
        stop::sleep(Duration::from_secs(3))?;
        c.cap(
            &run,
            "question-none-enter",
            "the question box after two Downs and Enter, 3 s later",
        )?;
        c.stub_log(&run, "question-none-enter");
    }
    // ---- a turn ended on each tool's own rows, no words after them
    for (name, tool, input) in TOOL_ROWS {
        let cap = format!("tool-row-{name}");
        if !any(&["tool-row", &cap]) {
            continue;
        }
        tool_row(c, &cap, tool, input)?;
    }
    // ---- the walls
    if any(&["rate-limit-retry", "rate-limit-final"]) {
        let run = c.fresh_muse(&["-c", "provider_retry.max_attempts=3"])?;
        c.mode(&run, "rate-limit")?;
        c.turn(&run, PROMPT)?;
        c.wait(&run, "retrying in", 30)?;
        c.cap(&run, "rate-limit-retry", "launched with -c provider_retry.max_attempts=3; the stub answers every turn 429 {error: {type: rate_limit_exceeded}} with retry-after: 5: the retry row")?;
        c.wait(&run, "attempts\\)|model failed", 60)?;
        stop::sleep(Duration::from_secs(1))?;
        c.cap(
            &run,
            "rate-limit-final",
            "the same turn once its third attempt was answered 429: the final error",
        )?;
        c.stub_log(&run, "rate-limit-final");
    }
    if want("401") {
        let run = c.fresh_muse(&[])?;
        c.mode(&run, "401")?;
        c.turn(&run, PROMPT)?;
        c.wait(&run, "rejected|401", 30)?;
        stop::sleep(Duration::from_secs(1))?;
        c.cap(&run, "401", "the stub answers the turn 401 {error: {type: invalid_request_error, code: invalid_api_key}}: the key's refusal (never retried)")?;
        c.stub_log(&run, "401");
    }
    if any(&["500-retry", "500-final"]) {
        let run = c.fresh_muse(&["-c", "provider_retry.max_attempts=2"])?;
        c.mode(&run, "500")?;
        c.turn(&run, PROMPT)?;
        c.wait(&run, "retrying in", 30)?;
        c.cap(&run, "500-retry", "launched with -c provider_retry.max_attempts=2; the stub answers every turn 500 {error: {type: server_error}}: the retry row")?;
        c.wait(&run, "attempts\\)|model failed", 60)?;
        stop::sleep(Duration::from_secs(1))?;
        c.cap(
            &run,
            "500-final",
            "the same turn once its second attempt was answered 500: the final error",
        )?;
        c.stub_log(&run, "500-final");
    }
    if want("limit-429") {
        let run = c.fresh_muse(&["-c", "provider_retry.max_attempts=1"])?;
        let reset = crate::util::now_secs() + 600;
        c.mode(&run, &format!("limit:five_hour:{reset}"))?;
        c.turn(&run, PROMPT)?;
        c.wait(&run, "429|[Ll]imit", 30)?;
        stop::sleep(Duration::from_secs(1))?;
        c.cap(&run, "limit-429", "launched with -c provider_retry.max_attempts=1; the stub's `limit:five_hour` mode answers the turn 429 {error: {type: usage_limit_reached, message, resets_at: 10 min from now, limit_window_minutes: 300}}: the usage wall")?;
    }
    // ---- /usage and /status, after one turn
    if any(&["usage", "status"]) {
        let run = c.fresh_muse(&[])?;
        c.turn(&run, PROMPT)?;
        c.wait(&run, REPLIED, 30)?;
        c.settle(&run)?;
        let door = Door::new(&run)?;
        for cmd in ["/usage", "/status"] {
            let name = &cmd[1..];
            if !want(name) {
                continue;
            }
            c.ctl(&run, &["send", cmd])?;
            stop::sleep(Duration::from_millis(1500))?;
            let screen = door.out(&["text"]);
            if !menu_offers_first(&screen, cmd) {
                eprintln!(
                    "aterm-lab: muse-captures: the menu does not offer {cmd} first: Enter not pressed, no {name}"
                );
                c.key(&run, "escape")?;
                c.ctl(&run, &["key", "ctrl+u"])?;
                continue;
            }
            c.key(&run, "enter")?;
            stop::sleep(Duration::from_secs(2))?;
            c.cap(
                &run,
                name,
                &format!("after one ended turn, `{cmd}` typed (the menu's first row) and Enter"),
            )?;
            c.key(&run, "escape")?;
            stop::sleep(Duration::from_secs(1))?;
        }
    }
    Ok(())
}

/// One of [`TOOL_ROWS`] captured as `name`: a fresh session in a project
/// holding [`README`], the stub's custom mode answering the turn with the one
/// call (`tool` with `input`) and its result with no words, the turn over
/// (the stub's log shows the call made, then the empty reply to its result),
/// the screen settled.
fn tool_row(c: &Cap, name: &str, tool: &str, input: &str) -> Result<(), Fail> {
    let run = c.fresh()?;
    c.project_file(&run, "README.md", README)?;
    let run = c.muse(&run, &["--trust-workspace"], "^❯")?;
    let input = crate::pyjson::J::parse(input)
        .map_err(|e| Fail::die(format!("muse-captures: {name}'s input: {e}")))?;
    let spec = crate::pyjson::J::obj([
        ("stream", crate::pyjson::J::Bool(true)),
        ("text", crate::pyjson::J::s("")),
        (
            "tool",
            crate::pyjson::J::obj([
                ("name", crate::pyjson::J::s(tool)),
                ("input", input.clone()),
            ]),
        ),
    ]);
    crate::verbs::set_custom_mode(&run, name, &spec)?;
    c.turn(&run, PROMPT)?;
    let log = run.w.join("stub.log");
    let called = format!(" custom tool:{tool}");
    let answered = |text: &str| tool_answered(text, &called);
    let mut over = false;
    for _ in 0..60 {
        if answered(&scratch_text(&log)) {
            over = true;
            break;
        }
        stop::sleep(Duration::from_millis(500))?;
    }
    c.must(
        over,
        &format!("the {tool} call and the empty reply to its result"),
    )?;
    c.settle(&run)?;
    c.cap(
        &run,
        name,
        &format!(
            "launched with --trust-workspace in a project whose README.md holds `{README}`; `{PROMPT}` and Enter, the stub's custom mode answering the turn with the one call `muse.{tool} {}` and its result with a reply holding no words: the turn over on the call's own rows, its run settled (5 s and a quiet screen)",
            input.dump(crate::pyjson::COMPACT)
        ),
    )?;
    c.stub_log(&run, name);
    Ok(())
}

/// Whether the stub's log `text` shows a turn's call (`called`, the log's `
/// custom tool:<name>`) made, and then a reply to a later request of the turn
/// with the custom mode's own text (` custom`, no call): its result answered.
#[must_use]
pub fn tool_answered(text: &str, called: &str) -> bool {
    let turns = || {
        text.lines()
            .filter(|l| l.contains("POST /responses") && l.contains(" kind=muse "))
    };
    let Some(at) = turns().position(|l| l.contains(called)) else {
        return false;
    };
    turns()
        .skip(at + 1)
        .any(|l| l.ends_with(" custom") || l.contains(" custom "))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tool row's capture waits for the call and the empty reply to its
    /// result, in that order, on the turn's own lines. NEGATIVE CONTROLS: the
    /// call alone, a side call's line, and the reply before the call.
    #[test]
    fn a_tool_rows_turn_is_over_once_its_result_was_answered() {
        let call = "1 POST /responses -> 200 body=9B mode=toolrow kind=muse auth=key model=lab-stub stream=true items=2 tools=1 instructions=0c last_user=\"x\" custom tool:search";
        let reply = "2 POST /responses -> 200 body=9B mode=toolrow kind=muse auth=key model=lab-stub stream=true items=4 tools=1 instructions=0c last_user=\"x\" custom";
        let side = "3 POST /responses -> 200 body=9B mode=toolrow kind=muse-side role=prompt-hint auth=key custom";
        assert!(tool_answered(
            &format!("{call}\n{reply}\n"),
            " custom tool:search"
        ));
        assert!(!tool_answered(&format!("{call}\n"), " custom tool:search"));
        assert!(!tool_answered(
            &format!("{call}\n{side}\n"),
            " custom tool:search"
        ));
        assert!(!tool_answered(
            &format!("{reply}\n{call}\n"),
            " custom tool:search"
        ));
        assert!(!tool_answered(
            &format!("{call}\n{reply}\n"),
            " custom tool:bash"
        ));
    }

    /// Enter after `/usage` only when the menu's first row IS `/usage` and the
    /// composer reads exactly that. NEGATIVE CONTROLS: a first row of another
    /// command, a longer one that starts with the same letters, and a composer
    /// that reads otherwise.
    #[test]
    fn a_slash_command_is_entered_only_when_the_menu_offers_it_first() {
        let rule = "─".repeat(60);
        let screen = |composer: &str, first: &str| {
            format!(
                "  Muse Code 1.4.10\n\n── Voice input (⌥ + v to start) {rule}\n❯ {composer}\n  {first}\n  /compact   Summarize\n{rule}\n  lab-stub · high · /p · Auto-review\n"
            )
        };
        assert!(menu_offers_first(
            &screen(
                "/usage",
                "/usage                Show session usage/cost/context"
            ),
            "/usage"
        ));
        assert!(!menu_offers_first(
            &screen(
                "/usage",
                "/upgrade              Show your subscription plan"
            ),
            "/usage"
        ));
        assert!(!menu_offers_first(
            &screen("/status", "/statusline   Choose the status line"),
            "/status"
        ));
        assert!(!menu_offers_first(
            &screen("/usag", "/usage                Show session usage"),
            "/usage"
        ));
        assert!(!menu_offers_first("no frame here", "/usage"));
    }
}
