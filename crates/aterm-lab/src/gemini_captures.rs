// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `gemini-captures [--only <regex>]` and `agy-captures [--only <regex>]`
//! (a few minutes each): every screen a harness needs of the REAL Gemini CLI
//! and the REAL agy ([`crate::gemini`]), drawn against the stub's Gemini API
//! and captured into `$LAB_STATE/captures/<agent>-<version>-<name>.txt` (+
//! png), the repo's fixture shape, on labs of their own (the harness off): the
//! main session's screens share one, every first run and every wall has a
//! fresh one. Run it on a new build of either before anything
//! presses a key on it. A step a capture's header names must have happened
//! ([`Cap::must`]): a turn the composer did not take, a screen that never
//! drew, a stub that did not take a mode ends the run, with nothing captured
//! on it.
//!
//! The names, the same for both agents where the screen is:
//!
//! * `first-run-trust` — launched `--untrusted`: the folder-trust dialog (the
//!   one seed a variant leaves out; the auth seed never is). agy also
//!   `first-run-color-scheme` and `first-run-terms`, launched `--onboarding`:
//!   its colour-scheme picker, then (one Enter, which keeps the default
//!   scheme) its Terms of Service, on which no key is pressed;
//! * the main session: `idle`, `draft` (typed, never sent), `busy-thinking`
//!   (the `slow` mode's thoughts), `busy-streaming` (its text), `end-of-turn`,
//!   `interrupted` (Esc while busy), `slash-menu` (`/` typed), `box-shell`,
//!   `box-write` (a new file), `box-edit` (that file), `question` and
//!   `question-answered` (the scripted calls of the `tool` mode, each box
//!   answered with Enter: its first option), agy's `survey` (its feedback
//!   survey, which it draws unasked, not every time, under an error turn of a
//!   session some turns old: eight ordinary turns after the boxes, then up to
//!   three 401 turns; skipped with `0` wherever it draws), and `exit`
//!   (`/quit`, with the resume hint each prints);
//! * the walls: Gemini CLI's `wall-429-per-day` (its usage-limit dialog on the
//!   per-day quota) and `wall-429-stopped` (after its Stop), agy's
//!   `error-429-per-day` (inline); `error-401`, `error-400-key`; `retry-503`
//!   and `retry-500` (the screen while the CLI retries the 503 or the 500),
//!   then Gemini CLI's `retry-503-recovered` / `retry-500-recovered` (the
//!   mode back to `ok`: it never stops retrying on its own) and agy's
//!   `error-503` / `error-500` (its retries run out: launched with
//!   `AGY_CLI_MODEL_API_MAX_RETRIES=2`, which the header says; agy does not
//!   retry a 500, so it has no `retry-500` and its `error-500` comes at once,
//!   which the run says).
//!
//! * Gemini CLI's modes, each on a lab of its own: `shell-mode-idle`,
//!   `-draft`, `-ran`, `-left` (`!`, a command typed, run with Enter, shell
//!   mode left with Esc); `yolo-idle`, `-draft`, `-busy`, `-end-of-turn`,
//!   `-tool-ran` (ctrl+y; a shell call run with no box); `details-hidden-idle`,
//!   `-draft`, `-busy`, `-end-of-turn`, `-box-shell`, `-yolo-idle`,
//!   `-yolo-draft` (Tab twice; the box refused with Esc); and, launched with
//!   `ui.showShortcutsHint` false in the project's `.gemini/settings.json`,
//!   `hint-off-idle`, `-busy`, `-end-of-turn`, `-details-hidden-idle`;
//! * launched with `ui.collapseDrawerDuringApproval` false, one lab:
//!   `drawer-kept-box-shell`, `-draft`, `-queued` (the shell box kept over
//!   the composer; `hello` typed into it; Enter, which queues it under the
//!   box), then, the box cancelled with Esc and the details hidden,
//!   `drawer-kept-details-hidden-box-shell`;
//! * Gemini CLI's other boxes, one lab: `question-multi`, `-second`,
//!   `-review`, `-answered` (a call asking two questions, each answered `1`,
//!   the review submitted with Enter), `box-sandbox-expansion` (a shell call
//!   naming `additional_permissions`), `box-web-fetch`, `box-mcp` (the lab's
//!   own stdio MCP server, `aterm-lab mcp-standin echo`, named in the
//!   project's settings), each refused with Esc;
//! * Gemini CLI in a pane of 72 columns: `narrow-error-401`, then with a
//!   `GEMINI.md` in the project `narrow-error-403`, `narrow-404-access-dialog`
//!   and `narrow-404-stopped` (its `2. Stop`);
//! * the Gemini API's other refusals ([`crate::stub::gemini::measured_refusals`],
//!   custom modes): agy's `error-<name>` each on a lab of its own (its
//!   fixtures carry `lab-` as its other lab captures do).
//!
//! What does not draw, measured on Gemini CLI 0.46.0 and agy 1.4.0: neither
//! draws a retry row (each holds its busy spinner while it retries a 503 or a
//! 500); Gemini CLI's 401 and 400 are inline `✕ [API Error: …]` rows, and
//! agy's every error an inline `⚠ agent executor error: …` block whose prompt
//! stays usable.

use std::cell::Cell;
use std::time::Duration;

use crate::door::Door;
use crate::gemini::{Agent, Launch};
use crate::lab::UpOpts;
use crate::run::Run;
use crate::util::scratch_text;
use crate::{Fail, LAB, Res, stop};

/// The captures' own lab and the agent they draw.
struct Cap {
    tag: String,
    agent: Agent,
    /// Whether agy's survey was captured already (it is skipped every time it draws).
    survey: Cell<bool>,
}

/// What an agent draws where the captures wait (each a regex of one row).
struct Screens {
    idle: &'static str,
    trust: &'static str,
    slash: &'static str,
    thinking: &'static str,
    streaming: &'static str,
    interrupted: &'static str,
    shell: &'static str,
    write: &'static str,
    edit: &'static str,
    question: &'static str,
    answered: &'static str,
    exit: &'static str,
}

const GEMINI: Screens = Screens {
    idle: "Type your message",
    trust: "Do you trust the files in this folder",
    slash: "\\(1/[0-9]+\\)",
    thinking: "Lab stub thinking",
    streaming: "✦ tick",
    interrupted: "Request cancelled",
    shell: "Allow execution of",
    write: "Writing to lab-write.txt",
    edit: "lab-write.txt: written by the lab stub =>",
    question: "Answer Questions",
    answered: "User answered",
    exit: "To resume this session",
};

const AGY: Screens = Screens {
    idle: "\\? for shortcuts",
    trust: "trust the contents",
    slash: "Navigate · enter Select",
    thinking: "tick [0-9]+\\.\\.\\.",
    streaming: "Thought for",
    interrupted: "Interrupted",
    shell: "Run this command\\?",
    write: "Allow creation of this file\\?",
    edit: "Accept this file edit\\?",
    question: "Question 1/1",
    answered: "^> Red",
    exit: "Resume with",
};

/// The `tool` scripts of the agent's four boxes: shell, write, edit, question.
fn scripts(agent: Agent) -> [&'static str; 4] {
    match agent {
        Agent::Gemini => [
            r#"{"name":"run_shell_command","input":{"command":"touch lab-probe.txt","description":"Make an empty lab-probe.txt in the project."}}"#,
            r#"{"name":"write_file","input":{"file_path":"@PROJ@/lab-write.txt","content":"written by the lab stub\nsecond line\n"}}"#,
            r#"{"name":"replace","input":{"file_path":"@PROJ@/lab-write.txt","instruction":"Change the first line.","old_string":"written by the lab stub","new_string":"edited by the lab stub"}}"#,
            r#"{"name":"ask_user","input":{"questions":[{"header":"Probe","question":"Which colour should the lab probe use?","type":"choice","options":[{"label":"Red","description":"A warm colour."},{"label":"Blue","description":"A cool colour."}]}]}}"#,
        ],
        Agent::Agy => [
            r#"{"name":"run_command","input":{"CommandLine":"touch lab-probe.txt","Cwd":"@PROJ@","WaitMsBeforeAsync":2000,"toolSummary":"Lab probe file","toolAction":"Creating the lab probe file"}}"#,
            r#"{"name":"write_to_file","input":{"TargetFile":"@PROJ@/lab-write.txt","Overwrite":true,"CodeContent":"written by the lab stub\nsecond line\n","Description":"Writes the lab probe file.","toolSummary":"Lab probe write","toolAction":"Writing the lab probe file"}}"#,
            r#"{"name":"replace_file_content","input":{"TargetFile":"@PROJ@/lab-write.txt","Instruction":"Change the first line.","Description":"Edits the lab probe file.","AllowMultiple":false,"TargetContent":"written by the lab stub","ReplacementContent":"edited by the lab stub","StartLine":1,"EndLine":1,"toolSummary":"Lab probe edit","toolAction":"Editing the lab probe file"}}"#,
            r#"{"name":"ask_question","input":{"questions":[{"question":"Which colour should the lab probe use?","options":["Red","Green","Blue"],"is_multi_select":false}],"toolSummary":"Colour preference","toolAction":"Asking for a colour"}}"#,
        ],
    }
}

/// agy's feedback survey, drawn unasked at the end of a turn.
const SURVEY: &str = "How's the CLI experience so far";

impl Cap {
    fn screens(&self) -> &'static Screens {
        match self.agent {
            Agent::Gemini => &GEMINI,
            Agent::Agy => &AGY,
        }
    }

    fn name(&self) -> &'static str {
        self.agent.name()
    }

    /// A lab of its own (the harness off), the previous one down.
    fn fresh(&self) -> Result<Run, Fail> {
        self.fresh_wide(None)
    }

    /// [`Cap::fresh`], `cols` columns wide when given.
    fn fresh_wide(&self, cols: Option<&str>) -> Result<Run, Fail> {
        let _ = crate::lab::down(&self.tag, false);
        let env = UpOpts::from_env();
        let o = UpOpts {
            harness_keys: "enabled = false".into(),
            lifeline: true,
            cols: cols.map_or(env.cols.clone(), str::to_string),
            ..env
        };
        crate::lab::up(&self.tag, &o).map_err(|f| {
            if f.stopped.is_some() {
                f
            } else {
                Fail::die(format!(
                    "{}-captures: up failed: {}",
                    self.name(),
                    f.msg.unwrap_or_default()
                ))
            }
        })
    }

    /// [`Cap::fresh`], then the agent launched with `l` and drawing.
    fn fresh_agent(&self, l: &Launch) -> Result<Run, Fail> {
        self.fresh_seeded(None, &[], l)
    }

    /// A lab `cols` wide (the default when `None`) whose project holds
    /// `files` (`(path in the project, text)`), then the agent launched with
    /// `l` and drawing.
    fn fresh_seeded(
        &self,
        cols: Option<&str>,
        files: &[(&str, String)],
        l: &Launch,
    ) -> Result<Run, Fail> {
        let run = self.fresh_wide(cols)?;
        for (rel, text) in files {
            project_file(&run, rel, text)?;
        }
        self.launch(run, l)
    }

    /// The agent launched with `l` in the lab `run`, and drawing.
    fn launch(&self, run: Run, l: &Launch) -> Result<Run, Fail> {
        crate::gemini::need(&run, self.agent)?;
        crate::gemini::launch(&run, self.agent, l).map_err(|f| {
            Fail::die(format!(
                "{}-captures: {} did not start: {}",
                self.name(),
                self.name(),
                f.msg.unwrap_or_default()
            ))
        })?;
        let run = Run::load(&self.tag)?;
        if !l.no_wait {
            let drawn = Door::new(&run)?.seen(self.agent.drawn(), 60)?;
            self.must(drawn, &format!("{} drawing", self.name()))?;
        }
        Ok(run)
    }

    /// Every step a capture's header names must have happened.
    fn must(&self, ok: bool, what: &str) -> Result<(), Fail> {
        if ok {
            Ok(())
        } else {
            Err(Fail::die(format!(
                "{}-captures: {what} did not happen, so nothing is captured on it ({LAB} text {})",
                self.name(),
                self.tag
            )))
        }
    }

    /// `ctl <args>` on the lab, which must succeed.
    fn ctl(&self, run: &Run, args: &[&str]) -> Result<(), Fail> {
        let ok = Door::new(run)?.ctl(args)?.code == 0;
        self.must(ok, &format!("ctl {}", args.join(" ")))
    }

    fn wait(&self, run: &Run, re: &str, secs: u64) -> Result<(), Fail> {
        let seen = Door::new(run)?.seen(re, secs)?;
        self.must(seen, &format!("wait '{re}'"))
    }

    /// The screen settled: no change for 1.5 s (a spinner never settles).
    fn settle(&self, run: &Run, secs: u64) -> Result<(), Fail> {
        let quiet = Door::new(run)?.await_idle(1500, secs)?;
        self.must(quiet, "the screen settling")
    }

    /// The screen now, as `ctl text` gives it.
    fn screen(&self, run: &Run) -> Result<String, Fail> {
        let r = Door::new(run)?.ctl(&["text"])?;
        self.must(r.code == 0, "ctl text")?;
        Ok(r.stdout)
    }

    /// agy's survey, when it stands on the screen: captured the first time,
    /// then skipped (`0`), so no typed prompt answers it.
    fn survey(&self, run: &Run) -> Result<(), Fail> {
        if self.agent != Agent::Agy || !self.screen(run)?.contains(SURVEY) {
            return Ok(());
        }
        if !self.survey.get() {
            self.cap(
                run,
                "survey",
                "agy's feedback survey, drawn unasked under the end of a turn (it drew under an error turn of a \
                 session some turns old, never under a first turn), its composer hidden: [1] Good [2] Fine [3] Bad \
                 [0] Skip; skipped with 0 after this capture",
            )?;
            self.survey.set(true);
        }
        self.ctl(run, &["key", "0"])?;
        let gone = Door::new(run)?.await_gone(SURVEY, 10)?;
        self.must(gone, "the survey skipped with 0")
    }

    fn turn(&self, run: &Run, text: &str) -> Result<(), Fail> {
        self.survey(run)?;
        let taken = crate::verbs::turn(&Door::new(run)?, text)?;
        self.must(taken, &format!("turn {text}"))
    }

    fn cap(&self, run: &Run, name: &str, what: &str) -> Result<(), Fail> {
        crate::verbs::capture(run, name, what)?;
        Ok(())
    }

    fn mode(&self, run: &Run, m: &str) -> Result<(), Fail> {
        crate::verbs::set_mode(run, m).map(drop)
    }

    /// A named key, which must be taken.
    fn key(&self, run: &Run, k: &str) -> Result<(), Fail> {
        self.ctl(run, &["key", k])
    }

    /// `text` typed and standing on the screen.
    fn typed(&self, run: &Run, text: &str) -> Result<(), Fail> {
        self.ctl(run, &["send", text])?;
        let mut stood = false;
        for _ in 0..20 {
            stop::sleep(Duration::from_millis(250))?;
            if self.screen(run)?.contains(text) {
                stood = true;
                break;
            }
        }
        self.must(stood, &format!("'{text}' standing on the screen"))
    }

    /// The spinner gone: a turn's end (the hint is drawn while it runs too).
    fn spun_down(&self, run: &Run, secs: u64) -> Result<(), Fail> {
        let gone = Door::new(run)?.await_gone("esc to cancel", secs)?;
        self.must(gone, "the turn's spinner gone")?;
        self.settle(run, 15)
    }

    /// How many answers of `status` the stub logged for a turn of this agent.
    fn answered(&self, run: &Run, status: u16) -> usize {
        let tail = format!(":streamGenerateContent -> {status} ");
        scratch_text(&run.w.join("stub.log"))
            .lines()
            .filter(|l| l.contains(&tail) && l.contains(" kind=gemini "))
            .count()
    }

    /// The composer's text now.
    fn composer(&self, run: &Run) -> Result<Option<String>, Fail> {
        Ok(crate::screen::composer_row(self.name(), &self.screen(run)?))
    }
}

/// `gemini-captures [--only <regex of names>]`.
pub fn cmd_gemini_captures(args: &[String]) -> Res {
    cmd(Agent::Gemini, args)
}

/// `agy-captures [--only <regex of names>]`.
pub fn cmd_agy_captures(args: &[String]) -> Res {
    cmd(Agent::Agy, args)
}

fn cmd(agent: Agent, args: &[String]) -> Res {
    let verb = format!("{}-captures", agent.name());
    let only = match args.first().map(String::as_str) {
        Some("--only") => args
            .get(1)
            .cloned()
            .ok_or_else(|| Fail::die(format!("{verb}: --only needs a pattern")))?,
        Some(other) => return Err(Fail::die(format!("{verb}: unknown option '{other}'"))),
        None => ".".into(),
    };
    let re = aterm_regex::Regex::new(&only).map_err(|e| Fail::die(format!("{verb}: {e}")))?;
    stop::install();
    let c = Cap {
        tag: verb.clone(),
        agent,
        survey: Cell::new(false),
    };
    let result = captures(&c, &|name: &str| re.is_match(name));
    let _ = crate::lab::down(&c.tag, false);
    let prefix = result?;
    let dir = crate::paths::paths().state.join("captures");
    let n = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
                .count()
        })
        .unwrap_or(0);
    println!("{verb}: done ({n} files {}/{prefix}*)", dir.display());
    Ok(0)
}

/// Every capture `want` names; the fixtures' prefix (`gemini-0.46.0-`).
fn captures(c: &Cap, want: &dyn Fn(&str) -> bool) -> Result<String, Fail> {
    let s = c.screens();
    // the version the fixtures are named after, as `up` reads it
    let prefix = {
        let run = c.fresh()?;
        crate::gemini::need(&run, c.agent)?;
        format!("{}-{}-", c.name(), c.agent.ver(&run))
    };
    println!(
        "{}-captures: {} -> $LAB_STATE/captures/{prefix}*",
        c.name(),
        prefix.trim_end_matches('-')
    );
    // ---- first runs: the seed a variant leaves out is the trust (and agy's onboarding), never the auth
    if c.agent == Agent::Agy && (want("first-run-color-scheme") || want("first-run-terms")) {
        let l = Launch {
            onboarding: true,
            untrusted: true,
            ..Launch::default()
        };
        let run = c.fresh_agent(&l)?;
        c.wait(&run, "Choose your color scheme", 30)?;
        c.settle(&run, 10)?;
        c.cap(&run, "first-run-color-scheme", "launched with no onboarding and no trust seed (the provider seeded: no sign-in): the first onboarding screen, before any key")?;
        if want("first-run-terms") {
            c.ctl(&run, &["key", "enter"])?;
            c.wait(&run, "Terms of Service", 15)?;
            c.settle(&run, 10)?;
            c.cap(&run, "first-run-terms", "the same first run after one Enter on the colour-scheme picker (its default, terminal): the Terms of Service and data-use screen, its box ticked by default; no key is pressed on it")?;
        }
    }
    if want("first-run-trust") {
        let l = Launch {
            untrusted: true,
            ..Launch::default()
        };
        let run = c.fresh_agent(&l)?;
        c.wait(&run, s.trust, 30)?;
        c.settle(&run, 10)?;
        c.cap(&run, "first-run-trust", "launched with no trust seed (the auth seeded: no sign-in), in the project dir: the folder-trust dialog, before any key")?;
    }
    // ---- the main session, one lab
    let main = [
        "idle",
        "draft",
        "busy-thinking",
        "busy-streaming",
        "end-of-turn",
        "interrupted",
        "slash-menu",
        "box-shell",
        "box-write",
        "box-edit",
        "question",
        "question-answered",
        "survey",
        "exit",
    ];
    if main.iter().any(|n| want(n)) {
        main_session(c, s, want)?;
    }
    // ---- the walls, each on a fresh lab
    walls(c, want)?;
    refusals(c, want)?;
    if c.agent == Agent::Gemini {
        gemini_modes(c, want)?;
        gemini_drawer_kept(c, want)?;
        gemini_boxes(c, want)?;
        gemini_narrow(c, want)?;
    }
    Ok(prefix)
}

/// Write `text` at `rel` in the lab's project (through no link: the
/// project's physical path).
fn project_file(run: &Run, rel: &str, text: &str) -> Result<(), Fail> {
    let proj = std::fs::canonicalize(&run.proj)
        .map_err(|e| Fail::die(format!("the project {}: {e}", run.proj)))?;
    let path = proj.join(rel);
    crate::sys::put(&path, 0o644, &crate::sys::PutSrc::Bytes(text.as_bytes()))
        .map_err(|e| Fail::die(format!("cannot write {}: {e}", path.display())))
}

/// The Gemini API's other refusals ([`crate::stub::gemini::measured_refusals`]):
/// agy's, each on a lab of its own, once its `Error ID` row draws (eight
/// retries of a 504 and of a bare 429 take about 150 s).
fn refusals(c: &Cap, want: &dyn Fn(&str) -> bool) -> Result<(), Fail> {
    if c.agent != Agent::Agy {
        return Ok(());
    }
    for (name, what, spec) in crate::stub::gemini::measured_refusals() {
        let capture = format!("error-{name}");
        if !want(&capture) {
            continue;
        }
        let run = c.fresh_agent(&Launch::default())?;
        c.wait(&run, c.screens().idle, 30)?;
        crate::verbs::set_custom_mode(&run, name, &spec)?;
        c.turn(&run, "carry on with the task")?;
        c.wait(&run, "^Error ID: ", 240)?;
        c.settle(&run, 15)?;
        let n = c.answered(&run, refusal_status(&spec));
        c.cap(
            &run,
            &capture,
            &format!(
                "a custom stub mode: {what}; {n} request(s) answered so before the error drew"
            ),
        )?;
    }
    Ok(())
}

/// The status a custom mode's spec answers with.
fn refusal_status(spec: &crate::pyjson::J) -> u16 {
    spec.get("status")
        .and_then(crate::pyjson::J::as_i64)
        .and_then(|s| u16::try_from(s).ok())
        .unwrap_or(200)
}

/// Gemini CLI's shell mode, YOLO, its UI details hidden and its shortcuts
/// hint off, each on a lab of its own (module header).
#[allow(clippy::too_many_lines)]
fn gemini_modes(c: &Cap, want: &dyn Fn(&str) -> bool) -> Result<(), Fail> {
    let s = c.screens();
    let any = |names: &[&str]| names.iter().any(|n| want(n));
    let [shell, ..] = scripts(c.agent);
    if any(&[
        "shell-mode-idle",
        "shell-mode-draft",
        "shell-mode-ran",
        "shell-mode-left",
    ]) {
        let run = c.fresh_agent(&Launch::default())?;
        c.wait(&run, s.idle, 30)?;
        c.settle(&run, 10)?;
        c.ctl(&run, &["send", "!"])?;
        c.wait(&run, "shell mode enabled", 10)?;
        c.settle(&run, 10)?;
        if want("shell-mode-idle") {
            c.cap(&run, "shell-mode-idle", "shell mode, `!` typed into the empty composer: its caret `!`, its placeholder `Type your shell command`, `shell mode enabled (esc to disable)` where the approval mode was")?;
        }
        c.typed(&run, "echo lab-shell-$((6*7))")?;
        if want("shell-mode-draft") {
            c.cap(
                &run,
                "shell-mode-draft",
                "shell mode holding a typed, unsent command",
            )?;
        }
        c.key(&run, "enter")?;
        c.wait(&run, "lab-shell-42", 15)?;
        c.settle(&run, 10)?;
        if want("shell-mode-ran") {
            c.cap(&run, "shell-mode-ran", "shell mode, `echo lab-shell-$((6*7))` sent with Enter: the command in a band with `$` for its caret, its output in a box, the shell-mode composer back")?;
        }
        c.key(&run, "escape")?;
        let gone = Door::new(&run)?.await_gone("shell mode enabled", 10)?;
        c.must(gone, "shell mode left with Esc")?;
        c.settle(&run, 10)?;
        if want("shell-mode-left") {
            c.cap(&run, "shell-mode-left", "the same, shell mode left with Esc: the command's band and its output above, the composer back at rest")?;
        }
    }
    if any(&[
        "yolo-idle",
        "yolo-draft",
        "yolo-busy",
        "yolo-end-of-turn",
        "yolo-tool-ran",
    ]) {
        let run = c.fresh_agent(&Launch::default())?;
        c.wait(&run, s.idle, 30)?;
        c.settle(&run, 10)?;
        c.key(&run, "ctrl+y")?;
        c.wait(&run, "YOLO Ctrl\\+Y", 10)?;
        c.settle(&run, 10)?;
        if want("yolo-idle") {
            c.cap(&run, "yolo-idle", "YOLO on (ctrl+y at rest): the composer's caret `*`, `YOLO Ctrl+Y` where the approval mode was")?;
        }
        c.typed(&run, "hello in yolo")?;
        if want("yolo-draft") {
            c.cap(&run, "yolo-draft", "YOLO on, a typed, unsent draft")?;
        }
        c.key(&run, "ctrl+u")?;
        c.mode(&run, "slow")?;
        c.turn(&run, "hello in yolo")?;
        c.wait(&run, s.thinking, 20)?;
        stop::sleep(Duration::from_secs(2))?;
        if want("yolo-busy") {
            c.cap(&run, "yolo-busy", "YOLO on, the draft sent (the stub's slow mode): the spinner on the status row, the sent message's band with its `>` caret")?;
        }
        c.spun_down(&run, 60)?;
        if want("yolo-end-of-turn") {
            c.cap(
                &run,
                "yolo-end-of-turn",
                "YOLO on, that turn's end: the reply, the spinner gone, the `*` composer",
            )?;
        }
        crate::verbs::set_tool(&run, &shell.replace("lab-probe.txt", "lab-yolo.txt"))?;
        c.turn(&run, "toolstart")?;
        c.wait(&run, "Tool call done", 30)?;
        c.settle(&run, 10)?;
        if want("yolo-tool-ran") {
            c.cap(&run, "yolo-tool-ran", "YOLO on, the stub's scripted `touch lab-yolo.txt` run with no box (the file made), the turn's reply under it")?;
        }
        c.mode(&run, "ok")?;
    }
    let hidden = [
        "details-hidden-idle",
        "details-hidden-draft",
        "details-hidden-busy",
        "details-hidden-end-of-turn",
        "details-hidden-box-shell",
        "details-hidden-yolo-idle",
        "details-hidden-yolo-draft",
    ];
    if any(&hidden) {
        let run = c.fresh_agent(&Launch::default())?;
        c.wait(&run, s.idle, 30)?;
        c.settle(&run, 10)?;
        // two Tabs at rest, one right after the other: the details hidden
        c.key(&run, "tab")?;
        c.key(&run, "tab")?;
        c.wait(&run, "press tab twice for more", 10)?;
        c.settle(&run, 10)?;
        if want("details-hidden-idle") {
            c.cap(&run, "details-hidden-idle", "the UI details hidden (Tab twice at rest): no rule, approval mode or footer, `press tab twice for more` on the status row right over the composer")?;
        }
        c.typed(&run, "a hidden draft")?;
        if want("details-hidden-draft") {
            c.cap(
                &run,
                "details-hidden-draft",
                "the UI details hidden, a typed, unsent draft",
            )?;
        }
        c.key(&run, "ctrl+u")?;
        c.mode(&run, "slow")?;
        c.turn(&run, "think slowly with the details hidden")?;
        c.wait(&run, s.thinking, 20)?;
        stop::sleep(Duration::from_secs(2))?;
        if want("details-hidden-busy") {
            c.cap(&run, "details-hidden-busy", "the UI details hidden, a turn thinking (the stub's slow mode): the spinner on the status row, in column 0, right over the composer")?;
        }
        c.spun_down(&run, 60)?;
        if want("details-hidden-end-of-turn") {
            c.cap(&run, "details-hidden-end-of-turn", "the UI details hidden, that turn's end: the reply, `press tab twice for more` back over the composer")?;
        }
        crate::verbs::set_tool(&run, &shell.replace("lab-probe.txt", "lab-hidden.txt"))?;
        c.turn(&run, "toolstart")?;
        c.wait(&run, s.shell, 30)?;
        c.settle(&run, 10)?;
        if want("details-hidden-box-shell") {
            c.cap(
                &run,
                "details-hidden-box-shell",
                "the UI details hidden, the shell tool's approval box",
            )?;
        }
        c.key(&run, "escape")?;
        let gone = Door::new(&run)?.await_gone(s.shell, 15)?;
        c.must(gone, "the shell box refused with Esc")?;
        c.mode(&run, "ok")?;
        c.settle(&run, 15)?;
        c.key(&run, "ctrl+y")?;
        c.wait(&run, "● YOLO", 10)?;
        c.settle(&run, 10)?;
        if want("details-hidden-yolo-idle") {
            c.cap(&run, "details-hidden-yolo-idle", "the UI details hidden with YOLO on (ctrl+y): `● YOLO` on the status row's left, the rule, `● YOLO` again on a row of its own, the `*` composer")?;
        }
        c.typed(&run, "yolo hidden draft")?;
        if want("details-hidden-yolo-draft") {
            c.cap(
                &run,
                "details-hidden-yolo-draft",
                "the UI details hidden with YOLO on, a typed, unsent draft",
            )?;
        }
    }
    let hint_off = [
        "hint-off-idle",
        "hint-off-busy",
        "hint-off-end-of-turn",
        "hint-off-details-hidden-idle",
    ];
    if any(&hint_off) {
        let settings = ".gemini/settings.json";
        let off = "{\"ui\": {\"showShortcutsHint\": false}}\n".to_string();
        let run = c.fresh_seeded(None, &[(settings, off)], &Launch::default())?;
        c.wait(&run, s.idle, 30)?;
        c.settle(&run, 10)?;
        if want("hint-off-idle") {
            c.cap(&run, "hint-off-idle", "`ui.showShortcutsHint` false (the project's .gemini/settings.json), a fresh session at rest: the status row blank in a wide pane")?;
        }
        c.mode(&run, "slow")?;
        c.turn(&run, "hint off turn")?;
        c.wait(&run, s.thinking, 20)?;
        stop::sleep(Duration::from_secs(2))?;
        if want("hint-off-busy") {
            c.cap(
                &run,
                "hint-off-busy",
                "`ui.showShortcutsHint` false, a turn thinking: the spinner, no hint",
            )?;
        }
        c.spun_down(&run, 60)?;
        if want("hint-off-end-of-turn") {
            c.cap(&run, "hint-off-end-of-turn", "`ui.showShortcutsHint` false, that turn's end: the reply, the status row blank again")?;
        }
        c.mode(&run, "ok")?;
        c.key(&run, "tab")?;
        c.key(&run, "tab")?;
        let gone = Door::new(&run)?.await_gone("workspace \\(/directory\\)", 10)?;
        c.must(gone, "the details hidden")?;
        c.settle(&run, 10)?;
        if want("hint-off-details-hidden-idle") {
            c.cap(&run, "hint-off-details-hidden-idle", "`ui.showShortcutsHint` false and the UI details hidden (Tab twice), at rest: a blank row right over the composer, nothing under it")?;
        }
    }
    Ok(())
}

/// Gemini CLI launched with `ui.collapseDrawerDuringApproval` false, the
/// shell box kept over the composer: as it comes up, with a draft typed into
/// the composer, with that draft queued by an Enter, then (Esc, the box
/// cancelled; Tab twice) again with the UI details hidden (module header).
fn gemini_drawer_kept(c: &Cap, want: &dyn Fn(&str) -> bool) -> Result<(), Fail> {
    let names = [
        "drawer-kept-box-shell",
        "drawer-kept-box-shell-draft",
        "drawer-kept-box-shell-queued",
        "drawer-kept-details-hidden-box-shell",
    ];
    if !names.iter().any(|n| want(n)) {
        return Ok(());
    }
    let s = c.screens();
    let [shell, ..] = scripts(c.agent);
    let shell = shell.replace("lab-probe.txt", "lab-drawer.txt");
    let kept = "{\"ui\": {\"collapseDrawerDuringApproval\": false}}\n".to_string();
    let run = c.fresh_seeded(None, &[(".gemini/settings.json", kept)], &Launch::default())?;
    c.wait(&run, s.idle, 30)?;
    c.settle(&run, 10)?;
    crate::verbs::set_tool(&run, &shell)?;
    c.turn(&run, "toolstart")?;
    c.wait(&run, s.shell, 30)?;
    c.settle(&run, 10)?;
    if want(names[0]) {
        c.cap(&run, names[0], "`ui.collapseDrawerDuringApproval` false (the project's .gemini/settings.json), the stub's scripted `touch lab-drawer.txt`: the shell box drawn over the composer it keeps, the status row blank over the rule")?;
    }
    c.typed(&run, "hello")?;
    if want(names[1]) {
        c.cap(
            &run,
            names[1],
            "the same, `hello` typed: the kept composer takes it, the box still pending over it",
        )?;
    }
    c.key(&run, "enter")?;
    c.wait(&run, "Queued \\(press", 10)?;
    c.settle(&run, 10)?;
    if want(names[2]) {
        c.cap(&run, names[2], "the same, `hello` typed and Enter pressed: the kept composer queues it (`Queued (press ↑ to edit):`) under the box, which stays pending")?;
    }
    c.key(&run, "escape")?;
    c.wait(&run, "Request cancelled", 15)?;
    c.mode(&run, "ok")?;
    c.settle(&run, 15)?;
    c.key(&run, "tab")?;
    c.key(&run, "tab")?;
    c.wait(&run, "press tab twice for more", 10)?;
    c.settle(&run, 10)?;
    crate::verbs::set_tool(&run, &shell)?;
    c.turn(&run, "toolstart")?;
    c.wait(&run, s.shell, 30)?;
    c.settle(&run, 10)?;
    if want(names[3]) {
        c.cap(&run, names[3], "`ui.collapseDrawerDuringApproval` false and the UI details hidden (Tab twice), the stub's scripted `touch lab-drawer.txt`: the shell box over the composer it keeps, a blank status row between, nothing under the band")?;
    }
    c.key(&run, "escape")?;
    c.mode(&run, "ok")
}

/// Gemini CLI's other boxes, on one lab whose project names the lab's own
/// MCP server (module header).
fn gemini_boxes(c: &Cap, want: &dyn Fn(&str) -> bool) -> Result<(), Fail> {
    let names = [
        "question-multi",
        "question-multi-second",
        "question-multi-review",
        "question-multi-answered",
        "box-sandbox-expansion",
        "box-web-fetch",
        "box-mcp",
    ];
    if !names.iter().any(|n| want(n)) {
        return Ok(());
    }
    let s = c.screens();
    // the server is this program, from the copy the jail runs (its run's bin/)
    let lab_bin = crate::paths::paths().run_dir(&c.tag).join("bin/aterm-lab");
    let mcp = crate::pyjson::J::obj([(
        "mcpServers",
        crate::pyjson::J::obj([(
            "lab",
            crate::pyjson::J::obj([
                ("command", crate::pyjson::J::s(lab_bin.to_string_lossy())),
                (
                    "args",
                    crate::pyjson::J::Arr(vec![
                        crate::pyjson::J::s("mcp-standin"),
                        crate::pyjson::J::s("echo"),
                    ]),
                ),
            ]),
        )]),
    )]);
    let run = c.fresh_seeded(
        None,
        &[(".gemini/settings.json", mcp.py())],
        &Launch::default(),
    )?;
    c.wait(&run, s.idle, 30)?;
    c.settle(&run, 15)?;
    let multi = r#"{"name":"ask_user","input":{"questions":[{"header":"Colour","question":"Which colour should the lab probe use?","type":"choice","options":[{"label":"Red","description":"A warm colour."},{"label":"Blue","description":"A cool colour."}]},{"header":"Size","question":"How big should the lab probe be?","type":"choice","options":[{"label":"Small","description":"A few bytes."},{"label":"Large","description":"A few kilobytes."}]}]}}"#;
    if names[..4].iter().any(|n| want(n)) {
        crate::verbs::set_tool(&run, multi)?;
        c.turn(&run, "toolstart")?;
        c.wait(&run, "Answer Questions", 30)?;
        c.settle(&run, 10)?;
        if want("question-multi") {
            c.cap(&run, "question-multi", "the ask-the-user tool with two questions (the stub's scripted call): the dialog's first question")?;
        }
        c.ctl(&run, &["send", "1"])?;
        c.wait(&run, "How big should the lab probe be", 10)?;
        c.settle(&run, 10)?;
        if want("question-multi-second") {
            c.cap(&run, "question-multi-second", "the same dialog after `1` on its first question: its second question, the first ticked in the tab row")?;
        }
        c.ctl(&run, &["send", "1"])?;
        c.wait(&run, "Review your answers", 10)?;
        c.settle(&run, 10)?;
        if want("question-multi-review") {
            c.cap(
                &run,
                "question-multi-review",
                "the same dialog after `1` on its second question: its review tab, both ticked",
            )?;
        }
        c.key(&run, "enter")?;
        c.wait(&run, s.answered, 15)?;
        c.settle(&run, 15)?;
        if want("question-multi-answered") {
            c.cap(&run, "question-multi-answered", "the same dialog submitted with Enter on its review tab: both answers under the tool's row, the turn's end")?;
        }
    }
    let refused = [
        (
            "box-sandbox-expansion",
            r#"{"name":"run_shell_command","input":{"command":"touch lab-sandbox.txt","description":"Make an empty lab-sandbox.txt.","additional_permissions":{"network":true,"fileSystem":{"read":["/tmp/lab-read"],"write":["/tmp/lab-write"]}}}}"#.to_string(),
            "allow access to the following",
            "the sandbox-expansion box (no sandbox: the stub's scripted shell call carrying `additional_permissions`, network and a read and a write path)",
        ),
        (
            "box-web-fetch",
            r#"{"name":"web_fetch","input":{"prompt":"Summarize https://example.com/lab-page in one line."}}"#.to_string(),
            "Do you want to proceed",
            "the web-fetch box (the stub's scripted `web_fetch` call, its prompt naming one URL): `Action Required` over the tool's row, its prompt, the URLs, `Do you want to proceed?`",
        ),
        (
            "box-mcp",
            format!(
                r#"{{"name":"mcp_lab_{}","input":{{"text":"hello from the lab"}}}}"#,
                crate::codex_rig::MCP_ECHO_TOOL
            ),
            "Allow execution of MCP tool",
            "an MCP tool's box (the lab's stdio MCP server, `aterm-lab mcp-standin echo`, named `lab` in the project's .gemini/settings.json; the stub's scripted call to its `lab_echo`): `Action Required` over the tool's row, the server and tool, `Allow execution of MCP tool …?`",
        ),
    ];
    for (name, script, re, what) in &refused {
        if !want(name) {
            continue;
        }
        crate::verbs::set_tool(&run, script)?;
        c.turn(&run, "toolstart")?;
        c.wait(&run, re, 30)?;
        c.settle(&run, 10)?;
        c.cap(&run, name, what)?;
        c.key(&run, "escape")?;
        let gone = Door::new(&run)?.await_gone(re, 15)?;
        c.must(gone, &format!("the {name} refused with Esc"))?;
        c.settle(&run, 15)?;
    }
    c.mode(&run, "ok")
}

/// Gemini CLI in a pane of 72 columns: a 401, then with a `GEMINI.md` a 403
/// and a 404's no-access dialog and its `Stop` (module header).
fn gemini_narrow(c: &Cap, want: &dyn Fn(&str) -> bool) -> Result<(), Fail> {
    let s = c.screens();
    if want("narrow-error-401") {
        let run = c.fresh_seeded(Some("72"), &[], &Launch::default())?;
        c.wait(&run, s.idle, 30)?;
        c.mode(&run, "401")?;
        c.turn(&run, "hi")?;
        c.wait(&run, "UNAUTHENTICATED", 30)?;
        c.settle(&run, 15)?;
        c.cap(&run, "narrow-error-401", "the pane at 72 columns, a 401 on the first turn: `✕ [API Error: …]` wrapped over rows two columns in, the `ℹ` notice under it wrapped too")?;
    }
    let refusal = |name: &str| {
        crate::stub::gemini::measured_refusals()
            .into_iter()
            .find(|(n, ..)| *n == name)
            .ok_or_else(|| Fail::die(format!("no measured refusal {name}")))
    };
    let context = "# Lab project\n\nA scratch project of the lab.\n".to_string();
    if want("narrow-error-403") {
        let run = c.fresh_seeded(
            Some("72"),
            &[("GEMINI.md", context.clone())],
            &Launch::default(),
        )?;
        c.wait(&run, s.idle, 30)?;
        let (name, what, spec) = refusal("perm403")?;
        crate::verbs::set_custom_mode(&run, name, &spec)?;
        c.turn(&run, "carry on after perm403")?;
        c.wait(&run, "PERMISSION_DENIED", 30)?;
        c.settle(&run, 15)?;
        c.cap(&run, "narrow-error-403", &format!("the pane at 72 columns with a GEMINI.md, a custom stub mode: {what} (not retried: one request)"))?;
    }
    if want("narrow-404-access-dialog") || want("narrow-404-stopped") {
        let run = c.fresh_seeded(Some("72"), &[("GEMINI.md", context)], &Launch::default())?;
        c.wait(&run, s.idle, 30)?;
        let (name, what, spec) = refusal("notfound404")?;
        crate::verbs::set_custom_mode(&run, name, &spec)?;
        c.turn(&run, "carry on after notfound404")?;
        c.wait(&run, "have access to", 30)?;
        c.settle(&run, 15)?;
        if want("narrow-404-access-dialog") {
            c.cap(&run, "narrow-404-access-dialog", &format!("the pane at 72 columns with a GEMINI.md, a custom stub mode: {what}: its no-access dialog (`It seems like you don't have access to …`, `1. Keep trying`, `2. Stop`) before any key"))?;
        }
        c.ctl(&run, &["send", "2"])?;
        let gone = Door::new(&run)?.await_gone("have access to", 15)?;
        c.must(gone, "the no-access dialog answered 2. Stop")?;
        c.settle(&run, 15)?;
        if want("narrow-404-stopped") {
            c.cap(
                &run,
                "narrow-404-stopped",
                "the same dialog answered `2. Stop`",
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn main_session(c: &Cap, s: &Screens, want: &dyn Fn(&str) -> bool) -> Result<(), Fail> {
    let run = c.fresh_agent(&Launch::default())?;
    c.wait(&run, s.idle, 30)?;
    c.settle(&run, 15)?;
    if want("idle") {
        c.cap(
            &run,
            "idle",
            "a fresh session in the project dir, every first-run screen seeded away: idle, before any key",
        )?;
    }
    if want("draft") {
        let draft = "a draft the lab never sends";
        c.ctl(&run, &["send", draft])?;
        let mut stood = false;
        for _ in 0..20 {
            stop::sleep(Duration::from_millis(250))?;
            if c.composer(&run)?.as_deref() == Some(draft) {
                stood = true;
                break;
            }
        }
        c.must(stood, "the draft standing in the composer")?;
        c.cap(
            &run,
            "draft",
            &format!("idle with a draft typed into the composer, never sent: '{draft}'"),
        )?;
        // ctrl+u takes the line; a composer that keeps it loses it a character at a time
        c.ctl(&run, &["key", "ctrl+u"])?;
        stop::sleep(Duration::from_millis(500))?;
        if c.composer(&run)?.as_deref() == Some(draft) {
            for _ in 0..draft.chars().count() {
                c.ctl(&run, &["key", "backspace"])?;
            }
        }
        stop::sleep(Duration::from_millis(500))?;
        let left = c.composer(&run)?.unwrap_or_default();
        c.must(!left.contains("draft"), "the draft taken back out")?;
    }
    if want("busy-thinking") || want("busy-streaming") || want("end-of-turn") || want("interrupted")
    {
        c.mode(&run, "slow")?;
        c.turn(&run, "think slowly")?;
        c.wait(&run, s.thinking, 20)?;
        stop::sleep(Duration::from_secs(2))?;
        if want("busy-thinking") {
            c.cap(&run, "busy-thinking", "busy: the stub's `slow` turn sends one thought a second (`**Lab stub thinking**`, then `tick N`) for 15 s; the screen 2 s after the first thought drew")?;
        }
        c.wait(&run, s.streaming, 30)?;
        stop::sleep(Duration::from_secs(3))?;
        if want("busy-streaming") {
            c.cap(&run, "busy-streaming", "busy: the same turn after its thoughts, its text streaming one `tick N` a second; the screen 3 s into the text")?;
        }
        c.wait(&run, "tick 30", 30)?;
        c.settle(&run, 20)?;
        if want("end-of-turn") {
            c.cap(
                &run,
                "end-of-turn",
                "the end of the `slow` turn (its last text `tick 30`): idle again",
            )?;
        }
        if want("interrupted") {
            c.turn(&run, "think slowly again")?;
            c.wait(&run, s.thinking, 20)?;
            stop::sleep(Duration::from_secs(2))?;
            c.ctl(&run, &["key", "escape"])?;
            c.wait(&run, s.interrupted, 10)?;
            c.settle(&run, 10)?;
            c.cap(
                &run,
                "interrupted",
                "a `slow` turn interrupted by one Esc 2 s after its first thought drew",
            )?;
        }
        c.mode(&run, "ok")?;
    }
    if want("slash-menu") {
        c.survey(&run)?;
        c.ctl(&run, &["send", "/"])?;
        c.wait(&run, s.slash, 10)?;
        c.settle(&run, 10)?;
        c.cap(
            &run,
            "slash-menu",
            "idle, `/` typed into the composer: the slash-command menu",
        )?;
        c.ctl(&run, &["key", "backspace"])?;
        let gone = Door::new(&run)?.await_gone(s.slash, 10)?;
        c.must(gone, "the slash menu closed by a backspace")?;
    }
    let [shell, write, edit, question] = scripts(c.agent);
    let boxes = [
        (
            "box-shell",
            shell,
            s.shell,
            "the `tool` mode's call of the shell tool (`touch lab-probe.txt`): the approval box",
        ),
        (
            "box-write",
            write,
            s.write,
            "the `tool` mode's call of the write tool (a new file, lab-write.txt): the approval box",
        ),
        (
            "box-edit",
            edit,
            s.edit,
            "the `tool` mode's call of the edit tool (the first line of lab-write.txt, which the write before it made): the approval box",
        ),
        (
            "question",
            question,
            s.question,
            "the `tool` mode's call of the question tool (one choice: Red, then the others): the question dialog",
        ),
    ];
    // a box needs the ones before it (the edit needs the written file), so the sequence runs up to the last one wanted
    let last = boxes
        .iter()
        .rposition(|(name, ..)| want(name) || (*name == "question" && want("question-answered")));
    if let Some(last) = last {
        for (name, script, re, what) in &boxes[..=last] {
            crate::verbs::set_tool(&run, script)?;
            c.turn(&run, "toolstart")?;
            c.wait(&run, re, 30)?;
            c.settle(&run, 10)?;
            if want(name) {
                c.cap(&run, name, what)?;
            }
            // its first option (allow once, allow the creation, accept the edit, Red)
            c.ctl(&run, &["key", "enter"])?;
            let gone = Door::new(&run)?.await_gone(re, 15)?;
            c.must(gone, &format!("the {name} answered with Enter"))?;
            c.settle(&run, 30)?;
        }
        if want("question-answered") && last == boxes.len() - 1 {
            c.wait(&run, s.answered, 10)?;
            c.cap(
                &run,
                "question-answered",
                "the question answered with Enter (its first option, Red), the turn ended",
            )?;
        }
        c.mode(&run, "ok")?;
    }
    if c.agent == Agent::Agy && want("survey") && !c.survey.get() {
        // agy asks for feedback unasked, not every time, under an error turn of a session some turns old (never
        // under a first turn's; it drew under one after nine turns, and under none after three): eight ordinary
        // turns, then up to three error turns
        for i in 1..=8 {
            c.turn(&run, &format!("an ordinary turn {i}"))?;
            c.settle(&run, 30)?;
        }
        c.mode(&run, "401")?;
        for i in 1..=3 {
            if c.survey.get() {
                break;
            }
            c.turn(&run, &format!("carry on after error {i}"))?;
            c.settle(&run, 30)?;
            if Door::new(&run)?.seen(SURVEY, 15)? {
                c.survey(&run)?;
            }
        }
        if !c.survey.get() {
            eprintln!("aterm-lab: agy-captures: no survey: agy drew none under three error turns");
        }
        c.mode(&run, "ok")?;
    }
    if want("exit") {
        c.turn(&run, "/quit")?;
        c.wait(&run, s.exit, 30)?;
        c.settle(&run, 15)?;
        c.cap(&run, "exit", "`/quit` typed and sent: the agent exits to the lab's shell (`lab$`), the resume hint it prints above it")?;
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn walls(c: &Cap, want: &dyn Fn(&str) -> bool) -> Result<(), Fail> {
    let s = c.screens();
    let wall = |name: &str, mode: &str, re: &str, what: &str| -> Result<(), Fail> {
        if !want(name) {
            return Ok(());
        }
        let run = c.fresh_agent(&Launch::default())?;
        c.wait(&run, s.idle, 30)?;
        c.mode(&run, mode)?;
        c.turn(&run, "carry on with the task")?;
        c.wait(&run, re, 30)?;
        c.settle(&run, 15)?;
        c.cap(&run, name, what)
    };
    if c.agent == Agent::Gemini && (want("wall-429-per-day") || want("wall-429-stopped")) {
        let run = c.fresh_agent(&Launch::default())?;
        c.wait(&run, s.idle, 30)?;
        c.mode(&run, "rate-limit")?;
        c.turn(&run, "carry on with the task")?;
        c.wait(&run, "Usage limit reached", 30)?;
        c.settle(&run, 15)?;
        let h = "the stub's `rate-limit`: 429 RESOURCE_EXHAUSTED, QuotaFailure quotaId \
                 GenerateRequestsPerDayPerProjectPerModel-FreeTier, RetryInfo 3600s";
        if want("wall-429-per-day") {
            c.cap(
                &run,
                "wall-429-per-day",
                &format!("{h}, on the first turn: the usage-limit dialog, before any key"),
            )?;
        }
        if want("wall-429-stopped") {
            // its second option, Stop (the first, Keep trying, sends the turn again)
            c.ctl(&run, &["key", "down"])?;
            stop::sleep(Duration::from_millis(500))?;
            c.ctl(&run, &["key", "enter"])?;
            let gone = Door::new(&run)?.await_gone("Usage limit reached", 10)?;
            c.must(gone, "the usage-limit dialog answered Stop")?;
            c.settle(&run, 15)?;
            c.cap(
                &run,
                "wall-429-stopped",
                &format!("{h}: the dialog answered with Down and Enter (2. Stop)"),
            )?;
        }
    }
    if c.agent == Agent::Agy {
        wall(
            "error-429-per-day",
            "rate-limit",
            "Error 429",
            "the stub's `rate-limit`: 429 RESOURCE_EXHAUSTED, QuotaFailure quotaId \
             GenerateRequestsPerDayPerProjectPerModel-FreeTier, RetryInfo 3600s, on the first turn (not retried)",
        )?;
    }
    let (e401, e400) = match c.agent {
        Agent::Gemini => ("UNAUTHENTICATED", "API key not valid"),
        Agent::Agy => ("Error 401", "Error 400"),
    };
    wall(
        "error-401",
        "401",
        e401,
        "the stub's `401`: 401 UNAUTHENTICATED (Request had invalid authentication credentials), on the first turn (not retried)",
    )?;
    wall(
        "error-400-key",
        "bad-key",
        e400,
        "the stub's `bad-key`: 400 INVALID_ARGUMENT, ErrorInfo reason API_KEY_INVALID (what Google answers a key that is no key), on the first turn (not retried)",
    )?;
    for (status, mode, label) in [
        (
            503,
            "overloaded",
            "503 UNAVAILABLE `The model is overloaded`",
        ),
        (500, "500", "500 INTERNAL `An internal error has occurred`"),
    ] {
        let (retry, after) = (
            format!("retry-{status}"),
            match c.agent {
                Agent::Gemini => format!("retry-{status}-recovered"),
                Agent::Agy => format!("error-{status}"),
            },
        );
        if !want(&retry) && !want(&after) {
            continue;
        }
        // agy gives up after its retries: two of them, so it does within seconds (eight by default, ~150 s)
        let l = match c.agent {
            Agent::Gemini => Launch::default(),
            Agent::Agy => Launch {
                envs: vec!["AGY_CLI_MODEL_API_MAX_RETRIES=2".into()],
                ..Launch::default()
            },
        };
        let how = match c.agent {
            Agent::Gemini => String::new(),
            Agent::Agy => " (launched with AGY_CLI_MODEL_API_MAX_RETRIES=2)".to_string(),
        };
        let run = c.fresh_agent(&l)?;
        c.wait(&run, s.idle, 30)?;
        c.mode(&run, mode)?;
        c.turn(&run, "carry on with the task")?;
        // the second attempt, or (agy, which does not retry every status) the error at once
        let error = format!("Error {status}");
        let (mut attempts, mut failed) = (0, false);
        for _ in 0..40 {
            stop::sleep(Duration::from_millis(500))?;
            attempts = c.answered(&run, status);
            failed = c.agent == Agent::Agy && c.screen(&run)?.contains(&error);
            if attempts >= 2 || failed {
                break;
            }
        }
        c.must(
            attempts >= 2 || failed,
            &format!("a retry of the {status}, or its error"),
        )?;
        if attempts >= 2 && !failed {
            stop::sleep(Duration::from_secs(1))?;
            if want(&retry) {
                c.cap(&run, &retry, &format!("the stub's `{mode}`: {label} on every attempt{how}; the screen 1 s after the stub answered the turn's second attempt (no retry row draws)"))?;
            }
        } else if want(&retry) {
            eprintln!(
                "aterm-lab: {}-captures: no {retry}: {} did not retry the {status} ({attempts} attempt)",
                c.name(),
                c.name()
            );
        }
        if c.agent == Agent::Gemini {
            c.mode(&run, "ok")?;
            c.wait(&run, "reply [0-9]+\\)", 90)?;
            c.settle(&run, 15)?;
            if want(&after) {
                let n = c.answered(&run, status);
                c.cap(&run, &after, &format!("the stub's `{mode}` ({label}) for {n} attempts of the turn, then `ok`: the next attempt answered (the CLI never stops retrying on its own)"))?;
            }
        } else {
            c.wait(&run, &error, 120)?;
            c.settle(&run, 15)?;
            if want(&after) {
                let n = c.answered(&run, status);
                let ran = if n > 1 {
                    format!("the error once its retries ran out ({n} attempts)")
                } else {
                    "the error at once (not retried: one attempt)".to_string()
                };
                c.cap(
                    &run,
                    &after,
                    &format!("the stub's `{mode}`: {label} on every attempt{how}: {ran}"),
                )?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyjson::J;

    /// Each box's script is one call of the tool that draws it, in the shape
    /// the stub serves (`name`, `input`), the file the write makes the one
    /// the edit changes. NEGATIVE CONTROL: neither agent's script names a
    /// tool of the other's.
    #[test]
    fn each_box_is_one_call_of_the_agents_own_tool() {
        let gemini = ["run_shell_command", "write_file", "replace", "ask_user"];
        let agy = [
            "run_command",
            "write_to_file",
            "replace_file_content",
            "ask_question",
        ];
        for (agent, names, other) in [(Agent::Gemini, gemini, agy), (Agent::Agy, agy, gemini)] {
            let calls: Vec<J> = scripts(agent)
                .iter()
                .map(|s| J::parse(s).unwrap())
                .collect();
            let got: Vec<&str> = calls
                .iter()
                .map(|c| c.get("name").and_then(J::as_str).unwrap())
                .collect();
            assert_eq!(got, names, "{agent:?}");
            assert!(got.iter().all(|n| !other.contains(n)), "{agent:?}");
            assert!(
                calls
                    .iter()
                    .all(|c| matches!(c.get("input"), Some(J::Obj(_))))
            );
            let file = |c: &J| {
                let i = c.get("input").unwrap();
                i.get("file_path")
                    .or_else(|| i.get("TargetFile"))
                    .and_then(J::as_str)
                    .map(str::to_string)
            };
            assert_eq!(file(&calls[1]), file(&calls[2]), "{agent:?}");
            assert_eq!(file(&calls[1]).as_deref(), Some("@PROJ@/lab-write.txt"));
        }
    }

    /// Every row a capture waits for is a pattern the regex engine takes,
    /// and matches the row it was read off (Gemini CLI 0.46.0, agy 1.4.0).
    #[test]
    fn every_screen_a_capture_waits_for_matches_its_measured_row() {
        for s in [&GEMINI, &AGY] {
            for re in [
                s.idle,
                s.trust,
                s.slash,
                s.thinking,
                s.streaming,
                s.interrupted,
                s.shell,
                s.write,
                s.edit,
                s.question,
                s.answered,
                s.exit,
            ] {
                assert!(aterm_regex::Regex::new(re).is_ok(), "{re}");
            }
        }
        let m = |re: &str, row: &str| aterm_regex::Regex::new(re).unwrap().is_match(row);
        assert!(m(GEMINI.slash, " (1/41)"));
        assert!(m(
            GEMINI.thinking,
            " ⠋ Lab stub thinking (esc to cancel, 3s)"
        ));
        assert!(m(
            GEMINI.edit,
            "│ ? Edit  lab-write.txt: written by the lab stub => edited by the lab stub"
        ));
        assert!(m(AGY.thinking, "⣽  tick 6..."));
        assert!(
            !m(AGY.thinking, "  tick 16 tick 17"),
            "the streamed text is no thought"
        );
        assert!(m(AGY.answered, "> Red"));
        assert!(m(AGY.idle, "? for shortcuts      Gemini 3.1 Pro · low"));
        assert!(m(
            SURVEY,
            " How's the CLI experience so far? Help us improve:"
        ));
        // the new steps' waits, on the rows they were measured on
        assert!(m("YOLO Ctrl\\+Y", " YOLO Ctrl+Y"));
        assert!(!m("YOLO Ctrl\\+Y", " Shift+Tab to accept edits"));
        assert!(m(
            "^Error ID: ",
            "Error ID: 5eed0000-1111-4222-8333-444455556666-1"
        ));
        assert!(!m("^Error ID: ", "  Error ID: quoted in a reply"));
        assert!(m(
            "workspace \\(/directory\\)",
            " workspace (/directory)        sandbox"
        ));
        assert!(m("Queued \\(press", "  Queued (press ↑ to edit):"));
        assert!(!m("Queued \\(press", "  Queued:"));
    }

    /// A refusal's capture counts the answers of the status its spec gives;
    /// a spec with none is a 200.
    #[test]
    fn a_refusal_is_counted_by_its_own_status() {
        for (name, _, spec) in crate::stub::gemini::measured_refusals() {
            let want = spec
                .get("json")
                .and_then(|j| j.get("error"))
                .and_then(|e| e.get("code"))
                .and_then(J::as_i64)
                .and_then(|c| u16::try_from(c).ok());
            assert_eq!(Some(refusal_status(&spec)), want, "{name}");
        }
        assert_eq!(refusal_status(&J::obj([("text", J::s("hi"))])), 200);
    }
}
