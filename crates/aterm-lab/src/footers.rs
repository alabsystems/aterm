// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Records Claude Code footer screens across modes and settings for fixture verification.

use std::path::PathBuf;

use crate::door::Door;
use crate::lab::{Launch, UpOpts};
use crate::pyjson::{COMPACT, J};
use crate::run::Run;
use crate::{Fail, Res};

/// Returns the mode word named by the row under the composer, if recognized.
#[must_use]
pub fn mode_word(screen: &str) -> Option<&'static str> {
    let row = screen
        .lines()
        .rev()
        .find(|l| l.contains("⏵⏵ ") || l.contains("⏸ "))?;
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

/// Returns the footer's last row as a pattern for `await gone`.
#[must_use]
pub fn footer_row_pattern(screen: &str) -> Option<String> {
    screen
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(aterm_phase::anchors::guard_regex)
}

/// `✻ <Verb> for <N>…`: a turn's done row (`screen::is_done_row`), as a pattern.
const DONE_ROW: &str = "✻.\\S+.for.[0-9]";

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
    /// `--columns` (80).
    pub cols: String,
    /// `--lines` (24).
    pub lines: String,
    /// `--subscriber`: the fake claude.ai login.
    pub subscriber: bool,
    /// `--settings '<json>'`: merged into the scratch settings before the first launch.
    pub settings: Option<J>,
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
            cols: "80".into(),
            lines: "24".into(),
            subscriber: false,
            settings: None,
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

struct Rec<'a> {
    run: &'a Run,
    door: Door<'a>,
    out: PathBuf,
    variant: String,
    unread: u32,
}

impl Rec<'_> {
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

    /// Press shift+tab and wait for the mode row to change; whether it did.
    fn shift_tab(&self) -> Result<bool, Fail> {
        let before = footer_row_pattern(&self.text());
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
        let _ = door.ctl(&["key", "down"]);
        let _ = door.ctl(&["key", "enter"]);
    }
    if !door.await_match("shift.tab|for.agents|shortcuts", 60)? {
        return Err(Fail::exit(
            3,
            "footers: Claude Code drew no footer within 60 s",
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
    launch(run, &rec.door, o.subscriber, &[])?;
    let start = rec.mode_or_unread();
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
    let mut seen = vec![start.clone()];
    for _ in 0..6 {
        if !rec.shift_tab()? {
            break;
        }
        let m = mode_word(&rec.text()).map(str::to_string);
        if m.as_ref().is_some_and(|m| seen.contains(m)) {
            break;
        }
        let m = m.unwrap_or_else(|| {
            rec.unread += 1;
            format!("unread-{}", rec.unread)
        });
        rec.snap(
            &format!("footer-{m}"),
            "after shift+tab: the next mode of the launch's cycle",
        )?;
        seen.push(m);
    }
    if o.turn {
        for _ in 0..6 {
            if mode_word(&rec.text()).map(str::to_string).as_ref() == Some(&start) {
                break;
            }
            if !rec.shift_tab()? {
                break;
            }
        }
        crate::verbs::set_mode(run, "slow")?;
        crate::verbs::turn(&rec.door, "count from 1 to 30")?;
        let busy = aterm_phase::anchors::guard_prefix("busy.interrupt", 3);
        if rec.door.await_match(&busy, 30)? {
            rec.snap(
                "footer-busy",
                &format!(
                    "mid-turn in the launch mode: {} on the mode row",
                    aterm_phase::anchor("busy.interrupt")
                ),
            )?;
        }
        rec.door.await_gone(&busy, 120)?;
        rec.door.await_match(DONE_ROW, 30)?;
        rec.settle()?;
        rec.snap("footer-after-turn", "idle after a turn in the launch mode")?;
        crate::verbs::set_mode(run, "ok")?;
    }
    quit(&rec.door)?;
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
        quit(&rec.door)?;
    }
    println!();
    println!(
        "Recorded Claude Code {} into {}.",
        run.claude_ver,
        rec.out.display()
    );
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
    fn the_footer_row_is_the_last_row_with_text_as_a_literal_pattern() {
        let screen = "❯ \n──────\n  ⏸ manual mode on · ? for shortcuts · ← for agents\n\n   \n";
        assert_eq!(
            footer_row_pattern(screen).as_deref(),
            Some("⏸.manual.mode.on.·.\\?.for.shortcuts.·.←.for.agents")
        );
        assert_eq!(footer_row_pattern("\n  \n"), None);
    }

    #[test]
    fn the_done_row_pattern_reads_what_the_done_row_reader_reads() {
        let re = aterm_regex::Regex::new(DONE_ROW).unwrap();
        for row in [
            "✻ Worked for 3m 2s · done 11:08 PM",
            "✻ Baked for 0s · done 5:39 AM",
        ] {
            assert!(re.is_match(row) && crate::screen::is_done_row(row), "{row}");
        }
        let busy = "✻ Working… (3s · ↓ 12 tokens)";
        assert!(!re.is_match(busy) && !crate::screen::is_done_row(busy));
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
        assert_eq!(Opts::parse(&a(&["--settings", "[1]"])).unwrap_err().code, 2);
        assert_eq!(Opts::parse(&a(&["--settings"])).unwrap_err().code, 2);
        assert_eq!(Opts::parse(&a(&["--bogus"])).unwrap_err().code, 2);
    }
}
