// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Records Claude Code footer screens across modes and settings for fixture verification.

use std::path::{Path, PathBuf};

use crate::door::Door;
use crate::lab::{Launch, UpOpts};
use crate::pyjson::{COMPACT, J};
use crate::run::Run;
use crate::{Fail, Res};

/// The row under the composer that names the mode: the last row with a pill glyph.
fn mode_row(screen: &str) -> Option<&str> {
    screen
        .lines()
        .rev()
        .find(|l| l.contains("⏵⏵ ") || l.contains("⏸ "))
}

/// Returns the mode word named by the row under the composer, if recognized.
#[must_use]
pub fn mode_word(screen: &str) -> Option<&'static str> {
    let row = mode_row(screen)?;
    [
        ("⏵⏵", "mode.bypass", "bypass"),
        ("⏵⏵", "mode.auto", "auto"),
        ("⏵⏵", "mode.accept_edits", "accept-edits"),
        ("⏵⏵", "mode.dont_ask", "dont-ask"),
        ("⏸", "mode.plan", "plan"),
        ("⏸", "mode.manual", "manual"),
    ]
    .into_iter()
    .find(|(glyph, id, _)| row.contains(&format!("{glyph} {}", aterm_phase::anchor(id))))
    .map(|(_, _, w)| w)
}

/// Returns the mode row ([`mode_word`]'s row) as a pattern for `await gone`: never another row
/// under it, such as Claude's effort hint wrapped onto a row of its own, which a press can leave
/// standing while the mode changes.
#[must_use]
pub fn mode_row_pattern(screen: &str) -> Option<String> {
    mode_row(screen)
        .map(str::trim)
        .map(aterm_phase::anchors::guard_regex)
}

/// `✻ <Verb> for <N>…`: a turn's done row (`screen::is_done_row`), as a pattern.
const DONE_ROW: &str = r"^✻ (?s:.*) for [0-9]";

/// How long the footer must stay unchanged to count as settled: its
/// first-render hints give way to the mode row within this.
const SETTLE_MS: u64 = 2000;

/// What `footers` was asked for.
#[derive(Debug)]
pub struct Opts {
    /// `--out DIR` (default: the lab's `captures/`).
    pub out: Option<PathBuf>,
    /// Without `--no-turn`.
    pub turn: bool,
    /// Without `--no-bypass`.
    pub bypass: bool,
    /// Without `--no-walk`: the first launch walks its ring, which can record an acceptance.
    pub walk: bool,
    /// `--walk-relaunch`: the relaunch walks its ring too, for a ring a resumed line holds.
    pub walk_relaunch: bool,
    /// `--probe`: run one scripted command in the mode the last launch rests in, and say whether
    /// it raised a box and whether it ran.
    pub probe: bool,
    /// `--probe-call '<json>'`: the tool call `--probe` runs, for one that is not its command.
    pub probe_call: Option<String>,
    /// `--columns` (80).
    pub cols: String,
    /// `--lines` (24).
    pub lines: String,
    /// `--subscriber`: the fake claude.ai login.
    pub subscriber: bool,
    /// `--settings '<json>'`: merged into the scratch settings before the first launch.
    pub settings: Option<J>,
    /// `--launch-args '<flags>'`: the first launch's flags, split on blanks (none by default).
    pub launch_args: Vec<String>,
    /// `--relaunch-args '<flags>'`: a further launch after the first ends, for the mode that
    /// launch lands in. `{session}` is the first launch's conversation (`--resume {session}`).
    pub relaunch_args: Option<Vec<String>>,
}

impl Opts {
    /// Parse `footers`' arguments.
    ///
    /// # Errors
    /// An unknown argument, a missing value, or settings that are not a JSON object.
    pub fn parse(args: &[String]) -> Result<Opts, Fail> {
        let mut o = Opts {
            out: None,
            turn: true,
            bypass: true,
            walk: true,
            walk_relaunch: false,
            probe: false,
            probe_call: None,
            cols: "80".into(),
            lines: "24".into(),
            subscriber: false,
            settings: None,
            launch_args: Vec::new(),
            relaunch_args: None,
        };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut val = || {
                it.next()
                    .cloned()
                    .ok_or_else(|| Fail::usage(format!("footers: {a} needs a value")))
            };
            match a.as_str() {
                "--out" => o.out = Some(PathBuf::from(crate::paths::from_caller(&val()?))),
                "--no-turn" => o.turn = false,
                "--no-bypass" => o.bypass = false,
                "--no-walk" => o.walk = false,
                "--walk-relaunch" => o.walk_relaunch = true,
                "--probe" => o.probe = true,
                "--probe-call" => {
                    o.probe = true;
                    o.probe_call = Some(val()?);
                }
                "--columns" => o.cols = val()?,
                "--lines" => o.lines = val()?,
                "--subscriber" => o.subscriber = true,
                "--settings" => {
                    let j = J::parse(&val()?)
                        .map_err(|e| Fail::usage(format!("footers: --settings: {e}")))?;
                    if !matches!(j, J::Obj(_)) {
                        return Err(Fail::usage("footers: --settings wants a JSON object"));
                    }
                    o.settings = Some(j);
                }
                "--launch-args" => o.launch_args = split_flags(&val()?),
                "--relaunch-args" => o.relaunch_args = Some(split_flags(&val()?)),
                other => return Err(Fail::usage(format!("footers: unknown argument {other}"))),
            }
        }
        Ok(o)
    }

    /// The variant, as each fixture's `what` ends: the settings it ran with.
    #[must_use]
    pub fn variant(&self) -> String {
        match &self.settings {
            Some(j) => format!("settings {}", j.dump(COMPACT)),
            None => "the lab's default settings".into(),
        }
    }
}

fn split_flags(flags: &str) -> Vec<String> {
    flags.split_whitespace().map(str::to_string).collect()
}

/// Which cycle [`Rec::walk_cycle`] walks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cycle {
    /// The launch mode's, under the lab's settings.
    Launch,
    /// A `--dangerously-skip-permissions` launch's, from bypass: how many presses of a person's
    /// shift+tab a leave of bypass takes to reach each mode.
    FromBypass,
}

impl Cycle {
    /// The capture's name for `mode`, reached at `press`: `footer-<mode>` for the launch's cycle,
    /// `footer-bypass-then-<press>-<mode>` from bypass.
    fn name(self, press: u32, mode: &str) -> String {
        match self {
            Cycle::Launch => format!("footer-{mode}"),
            Cycle::FromBypass => format!("footer-bypass-then-{press}-{mode}"),
        }
    }

    /// The capture's name for the mode the cycle comes back round to at `press`, its closing
    /// edge: [`Self::name`] with `-again`.
    fn again(self, press: u32, mode: &str) -> String {
        format!("{}-again", self.name(press, mode))
    }

    /// What each capture of it is.
    const fn what(self) -> &'static str {
        match self {
            Cycle::Launch => "after shift+tab: the next mode of the launch's cycle",
            Cycle::FromBypass => {
                "after shift+tab from bypass: the next mode of a --dangerously-skip-permissions launch's cycle, numbered by presses"
            }
        }
    }
}

struct Rec<'a> {
    run: &'a Run,
    door: Door<'a>,
    out: PathBuf,
    variant: String,
    unread: u32,
}

impl Rec<'_> {
    /// One scripted command in the mode the session rests in: whether it asks, and whether it ran.
    fn probe_command(&mut self, call: Option<&str>) -> Result<(), Fail> {
        const MARKER: &str = "probe-ran.txt";
        const BOXES: [&str; 2] = ["Do you want to proceed", "Enter plan mode?"];
        const NOTICE: &str = "Enter to continue";
        let mode = self.mode_or_unread();
        let command = format!(
            r#"{{"name": "Bash", "input": {{"command": "touch @PROJ@/{MARKER}", "description": "Touch a marker"}}}}"#
        );
        let call = call.unwrap_or(&command);
        crate::verbs::set_tool(self.run, &format!(r#"[{call}, {{"text": "Probe over."}}]"#))?;
        crate::verbs::turn(&self.door, "toolstart probe")?;
        let answered = format!("{}|Probe over|{NOTICE}", BOXES.join("|").replace('?', "."));
        let mut settled = self.door.await_match(&answered, 30)?;
        if settled && self.text().contains(NOTICE) {
            println!("probe in {mode}: pressed through Claude's auto-mode billing notice");
            self.door.await_idle(SETTLE_MS, 30)?;
            let _ = self.door.ctl(&["key", "enter"])?;
            let answered = format!("{}|Probe over", BOXES.join("|").replace('?', "."));
            settled = self.door.await_match(&answered, 30)?;
        }
        let text = self.text();
        let boxed = settled && BOXES.iter().any(|b| text.contains(b));
        let ran = Path::new(&self.run.proj).join(MARKER).exists();
        println!(
            "probe in {mode}: raised a box: {boxed}; ran: {ran} · {}",
            self.variant
        );
        self.snap(
            "footer-probe",
            "one scripted command in the mode the session rests in",
        )?;
        if boxed {
            let _ = self.door.ctl(&["key", "escape"])?;
        }
        crate::verbs::set_mode(self.run, "ok")?;
        Ok(())
    }

    /// A slow turn, typed to its done row at rest. `mid`: snap the launch mode's mid-turn row.
    fn slow_turn(&mut self, mid: bool) -> Result<(), Fail> {
        crate::verbs::set_mode(self.run, "slow")?;
        crate::verbs::turn(&self.door, "count from 1 to 30")?;
        let busy = aterm_phase::anchors::guard_prefix("busy.interrupt", 3);
        if self.door.await_match(&busy, 30)? && mid {
            self.snap(
                "footer-busy",
                &format!(
                    "mid-turn in the launch mode: {} on the mode row",
                    aterm_phase::anchor!("busy.interrupt")
                ),
            )?;
        }
        self.door.await_gone(&busy, 120)?;
        self.door.await_match(DONE_ROW, 30)?;
        self.settle()?;
        crate::verbs::set_mode(self.run, "ok")?;
        Ok(())
    }

    fn text(&self) -> String {
        self.door.out(&["text"])
    }

    fn snap(&self, name: &str, what: &str) -> Result<(), Fail> {
        let rows: Vec<String> = self.text().lines().map(str::to_string).collect();
        let what = format!("{what} · {}", self.variant);
        let (p, _) = crate::verbs::write_fixture_rows_in(
            self.run, &self.door, &self.out, name, &what, &rows,
        )?;
        eprintln!("captured {}", p.display());
        Ok(())
    }

    /// The mode the screen shows, or the next `unread-<n>`.
    fn mode_or_unread(&mut self) -> String {
        mode_word(&self.text()).map_or_else(
            || {
                self.unread += 1;
                format!("unread-{}", self.unread)
            },
            str::to_string,
        )
    }

    /// Wait for the footer to stop changing (a slow one is captured as it is).
    fn settle(&self) -> Result<(), Fail> {
        self.door.await_idle(SETTLE_MS, 30)?;
        Ok(())
    }

    /// Press shift+tab from `start` until the cycle comes back round (six presses at most), and
    /// snap each mode it reaches ([`Cycle::name`]) and the one it comes back round to, its closing
    /// edge ([`Cycle::again`]). Whether the walk came round: a press that left the mode row
    /// unchanged, or six presses that never came round, stop it, said on stderr.
    fn walk_cycle(&mut self, start: &str, cycle: Cycle) -> Result<bool, Fail> {
        let mut seen = vec![start.to_owned()];
        for press in 1..=6 {
            if !self.shift_tab()? {
                eprintln!(
                    "footers: shift+tab {press} from {start} left the mode row unchanged; the walk stopped there"
                );
                return Ok(false);
            }
            let m = mode_word(&self.text()).map(str::to_string);
            if let Some(m) = m.as_ref().filter(|m| seen.contains(m)) {
                self.snap(&cycle.again(press, m), cycle.what())?;
                return Ok(true);
            }
            let m = m.unwrap_or_else(|| {
                self.unread += 1;
                format!("unread-{}", self.unread)
            });
            self.snap(&cycle.name(press, &m), cycle.what())?;
            seen.push(m);
        }
        eprintln!("footers: six presses from {start} never came back round; the walk stopped");
        Ok(false)
    }

    /// Press shift+tab and wait for the mode row to change; whether it did.
    fn shift_tab(&self) -> Result<bool, Fail> {
        let before = mode_row_pattern(&self.text());
        let _ = self.door.ctl(&["send", "\u{1b}[Z"])?;
        let moved = match before {
            Some(p) => self.door.await_gone(&p, 10)?,
            None => true,
        };
        self.settle()?;
        Ok(moved)
    }
}

/// Launch Claude Code (with `args`) and wait for its footer to settle.
fn launch(run: &Run, door: &Door<'_>, subscriber: bool, args: &[&str]) -> Result<(), Fail> {
    let l = Launch {
        no_wait: true,
        subscriber,
        args: args.iter().map(|a| (*a).to_string()).collect(),
        ..Launch::default()
    };
    crate::lab::launch_claude(run, &l)?;
    if door.await_match("shift.tab|for.agents|shortcuts|Yes,.I.accept", 60)?
        && door.out(&["text"]).contains("Yes, I accept")
    {
        println!("launch {args:?} raised the bypass warning; pressed through it");
        let _ = door.ctl(&["key", "down"]);
        let _ = door.ctl(&["key", "enter"]);
    }
    if !door.await_match("shift.tab|for.agents|shortcuts", 60)? {
        return Err(Fail::exit(
            3,
            format!(
                "footers: Claude Code drew no footer within 60 s; the screen:\n{}",
                door.out(&["text"])
            ),
        ));
    }
    door.await_match("⏵⏵.|⏸.", 15)?;
    door.await_idle(SETTLE_MS, 30)?;
    Ok(())
}

/// End the Claude Code in the tab (ctrl+c twice, a person's half second
/// apart) and wait for the shell.
fn quit(door: &Door<'_>) -> Result<(), Fail> {
    for _ in 0..2 {
        let _ = door.ctl(&["send", "\u{3}"]);
        crate::stop::sleep(std::time::Duration::from_millis(500))?;
    }
    door.await_match("lab\\$", 20)?;
    Ok(())
}

/// `--relaunch-args`' word for the first launch's conversation.
const SESSION: &str = "{session}";

/// The newest conversation's transcript under the run's scratch home.
fn newest_transcript(run: &Run) -> Option<PathBuf> {
    let projects = run.w.join("home/.claude/projects");
    let mut found = Vec::new();
    crate::util::walk_files(&projects, &mut found);
    found
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .filter(|p| p.parent().and_then(Path::parent) == Some(projects.as_path()))
        .max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok())
}

/// `args` with [`SESSION`] replaced by the newest conversation's id.
fn with_session(run: &Run, args: &[String]) -> Result<Vec<String>, Fail> {
    if !args.iter().any(|a| a == SESSION) {
        return Ok(args.to_vec());
    }
    let id = newest_transcript(run)
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .ok_or_else(|| Fail::die("footers: {session}: the first launch left no transcript"))?;
    Ok(args
        .iter()
        .map(|a| if a == SESSION { id.clone() } else { a.clone() })
        .collect())
}

/// The `permission-mode` rows of a transcript, oldest first: what a restart with no pill reads.
fn mode_rows_in(transcript: &str) -> Vec<String> {
    transcript
        .lines()
        .filter(|l| l.contains("\"permission-mode\""))
        .filter_map(|l| J::parse(l).ok())
        .filter(|row| row.get("type").and_then(J::as_str) == Some("permission-mode"))
        .filter_map(|row| {
            row.get("permissionMode")
                .and_then(J::as_str)
                .map(str::to_string)
        })
        .collect()
}

/// [`mode_rows_in`] of the newest conversation.
fn mode_rows(run: &Run) -> Vec<String> {
    newest_transcript(run)
        .and_then(|p| crate::util::read_regular_text(&p).ok())
        .map_or_else(Vec::new, |t| mode_rows_in(&t))
}

fn record(run: &Run, o: &Opts, out: PathBuf) -> Res {
    if let Some(patch) = &o.settings {
        crate::verbs::settings_merge(run, patch)?;
    }
    let mut rec = Rec {
        run,
        door: Door::new(run)?,
        out,
        variant: o.variant(),
        unread: 0,
    };
    let flags: Vec<&str> = o.launch_args.iter().map(String::as_str).collect();
    launch(run, &rec.door, o.subscriber, &flags)?;
    let start = rec.mode_or_unread();
    println!("launch {flags:?} starts in {start} · {}", rec.variant);
    rec.snap(
        &format!("footer-{start}-idle"),
        "the launch mode, idle, empty composer: the mode row the footer rewrites and the lights read",
    )?;
    let _ = rec.door.ctl(&["send", "draft text"])?;
    rec.door.await_match("draft.text", 10)?;
    rec.settle()?;
    let m = rec.mode_or_unread();
    rec.snap(
        &format!("footer-{m}-draft"),
        "the launch mode with a draft in the composer",
    )?;
    let _ = rec.door.ctl(&["key", "ctrl+u"])?;
    rec.door.await_gone("draft.text", 10)?;
    rec.settle()?;
    let mut whole = !o.walk || rec.walk_cycle(&start, Cycle::Launch)?;
    if o.turn {
        for _ in 0..6 {
            if mode_word(&rec.text()).map(str::to_string).as_ref() == Some(&start) {
                break;
            }
            if !rec.shift_tab()? {
                break;
            }
        }
        rec.slow_turn(true)?;
        rec.snap("footer-after-turn", "idle after a turn in the launch mode")?;
    }
    if o.relaunch_args.is_some() {
        println!(
            "transcript mode rows before the first launch ends: {:?}",
            mode_rows(run)
        );
    } else if o.probe {
        rec.probe_command(o.probe_call.as_deref())?;
    }
    quit(&rec.door)?;
    if let Some(args) = &o.relaunch_args {
        println!(
            "transcript mode rows after the first launch: {:?}",
            mode_rows(run)
        );
        let args = with_session(run, args)?;
        let flags: Vec<&str> = args.iter().map(String::as_str).collect();
        launch(run, &rec.door, o.subscriber, &flags)?;
        let landed = rec.mode_or_unread();
        println!("relaunch {flags:?} lands in {landed} · {}", rec.variant);
        println!(
            "transcript mode rows at its first rest: {:?}",
            mode_rows(run)
        );
        rec.snap(
            &format!("footer-relaunch-{landed}"),
            "the mode a further launch lands in after the first ended",
        )?;
        if o.walk_relaunch {
            whole &= rec.walk_cycle(&landed, Cycle::Launch)?;
        }
        if o.turn {
            rec.slow_turn(false)?;
            println!(
                "transcript mode rows after its first turn: {:?}",
                mode_rows(run)
            );
        }
        if o.probe {
            rec.probe_command(o.probe_call.as_deref())?;
        }
        quit(&rec.door)?;
        println!("transcript mode rows after it ends: {:?}", mode_rows(run));
    }
    if o.bypass {
        launch(
            run,
            &rec.door,
            o.subscriber,
            &["--dangerously-skip-permissions"],
        )?;
        rec.snap(
            "footer-bypass-idle",
            "bypass permissions (started with --dangerously-skip-permissions), idle",
        )?;
        let start = rec.mode_or_unread();
        whole &= rec.walk_cycle(&start, Cycle::FromBypass)?;
        quit(&rec.door)?;
    }
    println!();
    if whole {
        println!(
            "Recorded Claude Code {} into {}.",
            run.claude_ver,
            rec.out.display()
        );
    } else {
        println!(
            "Recorded part of Claude Code {} into {}: a cycle walk stopped before it came back round (said above), so its later modes are missing.",
            run.claude_ver,
            rec.out.display()
        );
    }
    if rec.unread > 0 {
        println!(
            "{} mode row(s) this verb could not name were saved as footer-unread-<n>: the canary will say whether aterm reads them.",
            rec.unread
        );
    }
    println!(
        "Next: targo --unverified test -p aterm-agent --lib harness::   (the drift canary judges every capture)"
    );
    Ok(0)
}

/// Runs `lab footers` to record Claude Code footer screens.
pub fn cmd_footers(args: &[String]) -> Res {
    let o = Opts::parse(args)?;
    crate::stop::install();
    let tag = format!("footers-{}", std::process::id());
    let mut u = UpOpts::from_env();
    u.trust = true;
    u.harness_keys = "enabled = false".into();
    u.lifeline = true;
    u.cols.clone_from(&o.cols);
    u.lines.clone_from(&o.lines);
    let run = crate::lab::up(&tag, &u)?;
    let out = o
        .out
        .clone()
        .unwrap_or_else(|| crate::paths::paths().state.join("captures"));
    let rc = std::fs::create_dir_all(&out)
        .map_err(|e| Fail::die(format!("cannot make {}: {e}", out.display())))
        .and_then(|()| record(&run, &o, out));
    let _ = crate::lab::down(&tag, false);
    rc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mode_row_names_its_mode() {
        assert_eq!(
            mode_word("x\n  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents\n"),
            Some("auto")
        );
        assert_eq!(
            mode_word("  ⏸ plan mode on (shift+tab to cycle)"),
            Some("plan")
        );
        assert_eq!(
            mode_word("  ⏵⏵ accept edits on (shift+tab to cycle)"),
            Some("accept-edits")
        );
        assert_eq!(mode_word("  ⏵⏵ something new on"), None);
        assert_eq!(mode_word("no footer"), None);
    }

    #[test]
    fn a_cycle_from_bypass_numbers_each_capture_by_its_press() {
        assert_eq!(Cycle::Launch.name(2, "plan"), "footer-plan");
        assert_eq!(
            Cycle::FromBypass.name(1, "manual"),
            "footer-bypass-then-1-manual"
        );
        assert_eq!(Cycle::Launch.again(5, "auto"), "footer-auto-again");
        assert_eq!(
            Cycle::FromBypass.again(5, "bypass"),
            "footer-bypass-then-5-bypass-again"
        );
    }

    /// The pattern a press waits on is the mode row's, as a literal, never the effort hint
    /// Claude wraps onto a row of its own under it (2.1.316's bypass-idle screen at 80 columns),
    /// which a press can leave standing while the mode changes.
    #[test]
    fn the_mode_row_is_the_pattern_a_press_waits_on() {
        let screen = "❯ \n──────\n  ⏸ manual mode on · ? for shortcuts · ← for agents\n\n   \n";
        assert_eq!(
            mode_row_pattern(screen).as_deref(),
            Some("⏸.manual.mode.on.·.\\?.for.shortcuts.·.←.for.agents")
        );
        let wrapped = "❯ \n──────\n  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents\n                                                              ◐ medium · /effort\n";
        let pattern = mode_row_pattern(wrapped).expect("the mode row");
        assert_eq!(
            pattern,
            "⏵⏵.bypass.permissions.on.\\(shift\\+tab.to.cycle\\).·.←.for.agents"
        );
        let re = aterm_regex::Regex::new(&pattern).unwrap();
        assert!(!re.is_match("◐ medium · /effort"), "never the effort row");
        assert!(
            !re.is_match("  ⏵⏵ auto mode on (shift+tab to cycle)  ◐ medium · /effort"),
            "gone once the mode changes, the hint kept"
        );
        assert_eq!(mode_row_pattern("\n  \n"), None);
    }

    #[test]
    fn the_done_row_pattern_reads_what_the_done_row_reader_reads() {
        let re = aterm_regex::Regex::new(DONE_ROW).unwrap();
        let wired = aterm_regex::Regex::new(&crate::door::wire_pattern(DONE_ROW)).unwrap();
        for row in [
            "✻ Worked for 3m 2s · done 11:08 PM",
            "✻ Baked for 0s · done 5:39 AM",
            "✻ Worked hard for 3s",
            "✻  for 1s",
            "✻ Worked for ever and for 9s",
        ] {
            assert!(re.is_match(row) && crate::screen::is_done_row(row), "{row}");
            assert!(wired.is_match(row), "{row}");
        }
        for not_done in [
            "✻ Working… (3s · ↓ 12 tokens)",
            "x ✻aWorkedXforY1",
            "  ✻ Worked for 3s",
            "✻\tWorked for 3s",
            "✻ Worked for ever",
        ] {
            assert!(
                !re.is_match(not_done) && !crate::screen::is_done_row(not_done),
                "{not_done}"
            );
            assert!(!wired.is_match(not_done), "{not_done}");
        }
    }

    #[test]
    fn mode_rows_are_read_oldest_first() {
        let transcript = concat!(
            "{\"type\":\"user\",\"note\":\"permission-mode\"}\n",
            "{\"type\":\"permission-mode\",\"permissionMode\":\"auto\"}\n",
            "not json \"permission-mode\"\n",
            "{\"type\":\"permission-mode\",\"permissionMode\":\"bypassPermissions\"}\n",
        );
        assert_eq!(mode_rows_in(transcript), ["auto", "bypassPermissions"]);
    }

    #[test]
    fn the_variant_is_parsed_and_named() {
        let a = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let o = Opts::parse(&a(&[])).unwrap();
        assert!(!o.subscriber && o.settings.is_none() && o.turn && o.bypass);
        assert_eq!(o.variant(), "the lab's default settings");
        let o = Opts::parse(&a(&[
            "--subscriber",
            "--settings",
            r#"{"effortLevel":"xhigh"}"#,
            "--no-turn",
        ]))
        .unwrap();
        assert!(o.subscriber && !o.turn);
        assert_eq!(o.variant(), r#"settings {"effortLevel":"xhigh"}"#);
        let o = Opts::parse(&a(&[
            "--launch-args",
            "--allow-dangerously-skip-permissions  --permission-mode plan",
            "--relaunch-args",
            "--continue",
        ]))
        .unwrap();
        assert_eq!(
            o.launch_args,
            a(&[
                "--allow-dangerously-skip-permissions",
                "--permission-mode",
                "plan"
            ])
        );
        assert_eq!(o.relaunch_args, Some(a(&["--continue"])));
        assert!(Opts::parse(&a(&[])).unwrap().relaunch_args.is_none());
        assert_eq!(Opts::parse(&a(&["--settings", "[1]"])).unwrap_err().code, 2);
        assert_eq!(Opts::parse(&a(&["--settings"])).unwrap_err().code, 2);
        assert_eq!(Opts::parse(&a(&["--bogus"])).unwrap_err().code, 2);
    }
}
