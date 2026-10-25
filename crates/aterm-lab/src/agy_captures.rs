// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `agy-captures [--only <regex>]` (a few minutes).

use std::cell::Cell;
use std::time::Duration;

use crate::agy::{Launch, NAME};
use crate::door::Door;
use crate::lab::UpOpts;
use crate::run::Run;
use crate::util::scratch_text;
use crate::{Fail, LAB, Res, stop};

/// The captures' own lab.
struct Cap {
    tag: String,
    /// Whether agy's survey was captured already (it is skipped every time it draws).
    survey: Cell<bool>,
}

/// What agy draws where the captures wait (each a regex of one row).
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

/// The `tool` scripts of agy's four boxes: shell, write, edit, question.
const SCRIPTS: [&str; 4] = [
    r#"{"name":"run_command","input":{"CommandLine":"touch lab-probe.txt","Cwd":"@PROJ@","WaitMsBeforeAsync":2000,"toolSummary":"Lab probe file","toolAction":"Creating the lab probe file"}}"#,
    r#"{"name":"write_to_file","input":{"TargetFile":"@PROJ@/lab-write.txt","Overwrite":true,"CodeContent":"written by the lab stub\nsecond line\n","Description":"Writes the lab probe file.","toolSummary":"Lab probe write","toolAction":"Writing the lab probe file"}}"#,
    r#"{"name":"replace_file_content","input":{"TargetFile":"@PROJ@/lab-write.txt","Instruction":"Change the first line.","Description":"Edits the lab probe file.","AllowMultiple":false,"TargetContent":"written by the lab stub","ReplacementContent":"edited by the lab stub","StartLine":1,"EndLine":1,"toolSummary":"Lab probe edit","toolAction":"Editing the lab probe file"}}"#,
    r#"{"name":"ask_question","input":{"questions":[{"question":"Which colour should the lab probe use?","options":["Red","Green","Blue"],"is_multi_select":false}],"toolSummary":"Colour preference","toolAction":"Asking for a colour"}}"#,
];

/// agy's feedback survey, drawn unasked at the end of a turn.
const SURVEY: &str = "How's the CLI experience so far";

impl Cap {
    const fn screens(&self) -> &'static Screens {
        &AGY
    }

    const fn name(&self) -> &'static str {
        NAME
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
        crate::agy::need(&run)?;
        crate::agy::launch(&run, l).map_err(|f| {
            Fail::die(format!(
                "{}-captures: {} did not start: {}",
                self.name(),
                self.name(),
                f.msg.unwrap_or_default()
            ))
        })?;
        let run = Run::load(&self.tag)?;
        if !l.no_wait {
            let drawn = Door::new(&run)?.seen(crate::agy::DRAWN, 60)?;
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
        if !self.screen(run)?.contains(SURVEY) {
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

    /// How many answers of `status` the stub logged for a turn of agy's.
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

/// `agy-captures [--only <regex of names>]`.
pub fn cmd_agy_captures(args: &[String]) -> Res {
    let verb = format!("{NAME}-captures");
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

/// Every capture `want` names; the fixtures' prefix (`agy-1.4.0-`).
fn captures(c: &Cap, want: &dyn Fn(&str) -> bool) -> Result<String, Fail> {
    let s = c.screens();
    let prefix = {
        let run = c.fresh()?;
        crate::agy::need(&run)?;
        format!("{}-{}-", c.name(), run.agy_ver)
    };
    println!(
        "{}-captures: {} -> $LAB_STATE/captures/{prefix}*",
        c.name(),
        prefix.trim_end_matches('-')
    );
    if want("first-run-color-scheme") || want("first-run-terms") {
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
    walls(c, want)?;
    refusals(c, want)?;
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
    let [shell, write, edit, question] = SCRIPTS;
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
    if want("survey") && !c.survey.get() {
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
    wall(
        "error-429-per-day",
        "rate-limit",
        "Error 429",
        "the stub's `rate-limit`: 429 RESOURCE_EXHAUSTED, QuotaFailure quotaId \
         GenerateRequestsPerDayPerProjectPerModel-FreeTier, RetryInfo 3600s, on the first turn (not retried)",
    )?;
    let (e401, e400) = ("Error 401", "Error 400");
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
        let (retry, after) = (format!("retry-{status}"), format!("error-{status}"));
        if !want(&retry) && !want(&after) {
            continue;
        }
        let l = Launch {
            envs: vec!["AGY_CLI_MODEL_API_MAX_RETRIES=2".into()],
            ..Launch::default()
        };
        let how = " (launched with AGY_CLI_MODEL_API_MAX_RETRIES=2)";
        let run = c.fresh_agent(&l)?;
        c.wait(&run, s.idle, 30)?;
        c.mode(&run, mode)?;
        c.turn(&run, "carry on with the task")?;
        let error = format!("Error {status}");
        let (mut attempts, mut failed) = (0, false);
        for _ in 0..40 {
            stop::sleep(Duration::from_millis(500))?;
            attempts = c.answered(&run, status);
            failed = c.screen(&run)?.contains(&error);
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyjson::J;

    /// Each box's script is one call of the tool that draws it, in the shape
    /// the stub serves (`name`, `input`), the file the write makes the one
    /// the edit changes.
    #[test]
    fn each_box_is_one_call_of_agys_own_tool() {
        let calls: Vec<J> = SCRIPTS.iter().map(|s| J::parse(s).unwrap()).collect();
        let got: Vec<&str> = calls
            .iter()
            .map(|c| c.get("name").and_then(J::as_str).unwrap())
            .collect();
        assert_eq!(
            got,
            [
                "run_command",
                "write_to_file",
                "replace_file_content",
                "ask_question",
            ]
        );
        assert!(
            calls
                .iter()
                .all(|c| matches!(c.get("input"), Some(J::Obj(_))))
        );
        let file = |c: &J| {
            let i = c.get("input").unwrap();
            i.get("TargetFile").and_then(J::as_str).map(str::to_string)
        };
        assert_eq!(file(&calls[1]), file(&calls[2]));
        assert_eq!(file(&calls[1]).as_deref(), Some("@PROJ@/lab-write.txt"));
    }

    /// Every row a capture waits for is a pattern the regex engine takes,
    /// and matches the row it was read off (agy 1.4.0).
    #[test]
    fn every_screen_a_capture_waits_for_matches_its_measured_row() {
        {
            let s = &AGY;
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
        assert!(m(
            "^Error ID: ",
            "Error ID: 5eed0000-1111-4222-8333-444455556666-1"
        ));
        assert!(!m("^Error ID: ", "  Error ID: quoted in a reply"));
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
