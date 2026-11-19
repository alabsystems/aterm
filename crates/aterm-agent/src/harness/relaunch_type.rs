// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RELAUNCH TYPER: the one line that starts an agent again, typed at its tab's shell prompt
//! under a writer's lane ([`Lane`]), fenced against a person's keys and a moved prompt.
//!
//! The built-in relaunch (`relaunch.rs`, each lane's drive) and aterm's harness watchdog
//! ([`type_run_line`], the HX-15 plan's D22) both type through it. It moved here out of
//! `relaunch.rs`, which HX-20 deletes; this module stays.

use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_json::Value;

use super::agent_image::Lane;
use super::relaunch::{NO_SHELL, line_grace, line_held_by_status, newest_block, pause, same_dir};
use super::upgrade::{self, Dialect};
use super::upgrade_drive::{
    Client, Opts, Records, Screen, Typed, conversation_live, screen, turn_fenced, type_line,
};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum RelaunchLineError {
    Wait(&'static str),
    /// A hand the line waits on stands on the tab ([`hand_on_tab`]: a hold, another driver's lease, a turn).
    Hand,
    Turn(String),
    /// The line's request went unanswered within its wait ([`Typed::Unanswered`]): it may have run.
    Unanswered,
}

/// Type the relaunch line. Only `upgrade_drive`'s tests call it, and they run on unix.
#[cfg(all(test, unix))]
#[allow(clippy::too_many_arguments)]
pub(super) fn type_relaunch_line(
    opts: &Opts,
    records: &Records,
    c: &mut Client,
    shell: u32,
    tab: &str,
    session: &str,
    line: &str,
    dialect: Option<Dialect>,
    look: PromptLook<'_>,
    tab_probe: impl FnMut(&mut Client, u32, &str) -> bool,
) -> Result<(), RelaunchLineError> {
    type_relaunch_line_admitted(
        opts,
        records,
        c,
        shell,
        tab,
        session,
        line,
        dialect,
        look,
        tab_probe,
        None,
        Lane::Relaunch,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn type_relaunch_line_admitted(
    opts: &Opts,
    records: &Records,
    c: &mut Client,
    shell: u32,
    tab: &str,
    session: &str,
    line: &str,
    dialect: Option<Dialect>,
    look: PromptLook<'_>,
    tab_probe: impl FnMut(&mut Client, u32, &str) -> bool,
    native: Option<(&super::native_act::Binding, super::native_act::ImagePhase)>,
    lane: Lane,
) -> Result<(), RelaunchLineError> {
    type_relaunch_line_admitted_with(
        opts,
        c,
        shell,
        tab,
        line,
        dialect,
        look,
        || {
            records
                .session_files()
                .is_some_and(|files| !conversation_live(&files, session))
        },
        tab_probe,
        native,
        lane,
    )
}

/// What one relaunch attempt judges the returned prompt by.
pub(super) struct PromptLook<'a> {
    pub(super) mark: &'a mut Option<PromptMark>,
    pub(super) ended_at_s: u64,
    /// Where a hand held the line ([`hand_held_since`]).
    pub(super) held_since_s: u64,
    /// The grace the record gives a person's keys over the line ([`St::line_grace_s`],
    /// [`line_grace`]).
    pub(super) line_grace_s: Option<u32>,
    /// Input accepted by the explicit act whose line is owed.
    pub(super) line_input: Option<&'a super::restart_stamp::Input>,
}

/// The one relaunch line, whichever agent it resumes, named as `lane`'s write.
#[allow(clippy::too_many_arguments)]
pub(super) fn type_relaunch_line_with(
    opts: &Opts,
    c: &mut Client,
    shell: u32,
    tab: &str,
    line: &str,
    dialect: Option<Dialect>,
    look: PromptLook<'_>,
    owner_clear: impl FnOnce() -> bool,
    tab_probe: impl FnMut(&mut Client, u32, &str) -> bool,
    lane: Lane,
) -> Result<(), RelaunchLineError> {
    type_relaunch_line_admitted_with(
        opts,
        c,
        shell,
        tab,
        line,
        dialect,
        look,
        owner_clear,
        tab_probe,
        None,
        lane,
    )
}

#[allow(clippy::too_many_arguments)]
fn type_relaunch_line_admitted_with(
    opts: &Opts,
    c: &mut Client,
    shell: u32,
    tab: &str,
    line: &str,
    dialect: Option<Dialect>,
    look: PromptLook<'_>,
    owner_clear: impl FnOnce() -> bool,
    mut tab_probe: impl FnMut(&mut Client, u32, &str) -> bool,
    native: Option<(&super::native_act::Binding, super::native_act::ImagePhase)>,
    lane: Lane,
) -> Result<(), RelaunchLineError> {
    let PromptLook {
        mark,
        ended_at_s,
        held_since_s,
        line_grace_s,
        line_input,
    } = look;
    if !tab_probe(c, shell, tab) {
        return Err(RelaunchLineError::Wait("tab-ownership-changed"));
    }
    if !owner_clear() {
        return Err(RelaunchLineError::Wait("conversation-owner-ambiguous"));
    }
    if !tab_probe(c, shell, tab) {
        return Err(RelaunchLineError::Wait("tab-ownership-changed"));
    }
    let checked_status = super::upgrade_drive::status_line(c, tab);
    let ours = super::upgrade_drive::hand_name();
    let accepted_input = line_input.is_some_and(|input| {
        super::restart_stamp::input_host().as_deref() == Some(input.host.as_str())
            && checked_status
                .as_deref()
                .and_then(super::native_restart::human_seq)
                == Some(input.seq)
    }) && (native.is_some() || super::upgrade_drive::turn_takes_human(c));
    let accepted_seq = line_input.filter(|_| accepted_input).map(|input| input.seq);
    if let Some(held) = line_held_by_status(
        checked_status.as_deref(),
        if accepted_input {
            0
        } else {
            line_grace(opts.human_grace_s, ended_at_s, line_grace_s)
        },
        &ours,
    ) {
        return Err(held);
    }
    let native = match native {
        None => None,
        Some((binding, phase)) => Some((
            binding,
            phase,
            checked_status
                .as_deref()
                .and_then(super::native_restart::human_seq)
                .ok_or(RelaunchLineError::Wait("native-person-unread"))?,
        )),
    };
    if mark.is_none()
        && let Some(scr) = screen(c, tab)
    {
        if accepted_seq.is_some() && scr.human_seq != accepted_seq {
            return Err(RelaunchLineError::Wait("held"));
        }
        *mark = first_mark(
            c,
            tab,
            &scr,
            ended_at_s,
            if accepted_input { 0 } else { held_since_s },
        )?;
    }
    let healed = native
        .is_none()
        .then(|| dialect.and_then(|d| healed_line(c, tab, shell, d, line)))
        .flatten();
    let typed = type_at_prompt(
        c,
        shell,
        tab,
        healed.as_deref().unwrap_or(line),
        mark,
        &mut tab_probe,
        opts.timings.fence_every,
        accepted_seq,
        native,
        lane,
    );
    if typed.is_err() && healed.is_some() {
        withdraw_rekey(c, tab);
    }
    typed
}

fn healed_line(
    c: &mut Client,
    tab: &str,
    shell: u32,
    dialect: Dialect,
    line: &str,
) -> Option<String> {
    let path = issue_rekey(c, tab, shell)?;
    match upgrade::with_rekey(dialect, line, &path) {
        Ok(healed) => Some(healed),
        Err(_) => {
            withdraw_rekey(c, tab);
            None
        }
    }
}

fn issue_rekey(c: &mut Client, tab: &str, shell: u32) -> Option<PathBuf> {
    let reply = c
        .request_line(&format!("@{tab} rekey shell={shell}"))
        .ok()?;
    rekey_path_of(&reply)
}

fn rekey_path_of(reply: &str) -> Option<PathBuf> {
    let rest = reply
        .trim_end_matches(['\r', '\n'])
        .strip_prefix("OK rekey ")?;
    let (_, path) = rest.split_once("path=")?;
    Path::new(path).is_absolute().then(|| PathBuf::from(path))
}

fn withdraw_rekey(c: &mut Client, tab: &str) {
    let _ = c.request_line(&format!("@{tab} rekey withdraw"));
}

fn refused_line(e: String) -> RelaunchLineError {
    if e.starts_with("ERR busy") {
        RelaunchLineError::Wait("held")
    } else if e.starts_with("ERR halted") {
        RelaunchLineError::Hand
    } else {
        RelaunchLineError::Turn(e)
    }
}

#[allow(clippy::too_many_arguments)]
fn type_at_prompt(
    c: &mut Client,
    shell: u32,
    tab: &str,
    line: &str,
    mark: &mut Option<PromptMark>,
    tab_probe: &mut impl FnMut(&mut Client, u32, &str) -> bool,
    fence_every: Duration,
    accepted_input: Option<u64>,
    native: Option<(
        &super::native_act::Binding,
        super::native_act::ImagePhase,
        u64,
    )>,
    lane: Lane,
) -> Result<(), RelaunchLineError> {
    if let Some((binding, phase, human_seq)) = native {
        let scr = screen(c, tab);
        if accepted_input.is_some() && scr.as_ref().and_then(|s| s.human_seq) != accepted_input {
            return Err(RelaunchLineError::Wait("held"));
        }
        at_mark(mark, scr.as_ref())?;
        if !tab_probe(c, shell, tab) {
            return Err(RelaunchLineError::Wait("tab-ownership-changed"));
        }
        let generation = scr
            .and_then(|s| s.generation)
            .ok_or(RelaunchLineError::Wait("native-screen-unread"))?;
        return super::native_restart::resume(c, tab, binding, phase, &generation, human_seq)
            .map_err(|error| match error {
                Typed::Unanswered => RelaunchLineError::Unanswered,
                _ => RelaunchLineError::Wait("native-continuation"),
            });
    }
    let bash = atpkg::caller_shell::process_args(shell)
        .and_then(|a| Dialect::from_exe_name(&a.exec_path))
        == Some(Dialect::Bash);
    if !bash {
        for _ in 0..RELAUNCH_FENCE_TRIES {
            let scr = screen(c, tab);
            at_mark(mark, scr.as_ref())?;
            if !tab_probe(c, shell, tab) {
                return Err(RelaunchLineError::Wait("tab-ownership-changed"));
            }
            let Some((generation, human)) = scr.and_then(|s| Some((s.generation?, s.human_seq)))
            else {
                break;
            };
            if accepted_input.is_some() && accepted_input != human {
                return Err(RelaunchLineError::Wait("held"));
            }
            match turn_fenced(
                c,
                tab,
                line,
                Some(&generation),
                accepted_input.or(human),
                lane,
            ) {
                Ok(()) => return Ok(()),
                Err(Typed::Changed) => pause(fence_every),
                Err(Typed::Yielded) => return Err(RelaunchLineError::Wait("yield")),
                Err(Typed::Person) => return Err(RelaunchLineError::Wait("held")),
                Err(Typed::Cut(_)) => return Err(RelaunchLineError::Wait("cut")),
                Err(Typed::Refused(e)) => return Err(refused_line(e)),
                Err(Typed::Unanswered) => return Err(RelaunchLineError::Unanswered),
            }
        }
    }
    let last = screen(c, tab);
    at_mark(mark, last.as_ref())?;
    if !tab_probe(c, shell, tab) {
        return Err(RelaunchLineError::Wait("tab-ownership-changed"));
    }
    let human = last.and_then(|s| s.human_seq);
    if accepted_input.is_some() && accepted_input != human {
        return Err(RelaunchLineError::Wait("held"));
    }
    let typed = if bash && accepted_input.is_none() {
        type_line(c, tab, line, lane)
    } else {
        turn_fenced(c, tab, line, None, accepted_input.or(human), lane)
    };
    match typed {
        Ok(()) => Ok(()),
        Err(Typed::Yielded | Typed::Changed) => Err(RelaunchLineError::Wait("yield")),
        Err(Typed::Person) => Err(RelaunchLineError::Wait("held")),
        Err(Typed::Cut(_)) => Err(RelaunchLineError::Wait("cut")),
        Err(Typed::Refused(e)) => Err(refused_line(e)),
        Err(Typed::Unanswered) => Err(RelaunchLineError::Unanswered),
    }
}

fn first_mark(
    c: &mut Client,
    tab: &str,
    scr: &Screen,
    ended_at_s: u64,
    held_since_s: u64,
) -> Result<Option<PromptMark>, RelaunchLineError> {
    let Some(mark) = PromptMark::of(scr) else {
        return Ok(None);
    };
    let row = scr
        .cursor
        .and_then(|(r, _)| scr.rows.get(r))
        .map_or("", String::as_str);
    if first_drawn(row, mark.col).is_some_and(|at| at < mark.col.saturating_add(2)) {
        return Err(RelaunchLineError::Wait("typing"));
    }
    if held_since_s != 0
        && scr
            .human
            .quiet_since(held_since_s.saturating_mul(1000), now_ms())
            != Some(true)
    {
        return Err(RelaunchLineError::Wait("keyed-while-held"));
    }
    let unkeyed = ended_at_s != 0
        && scr
            .human
            .quiet_since(ended_at_s.saturating_mul(1000), now_ms())
            == Some(true);
    match input_start(c, tab) {
        Some(start) if start.names(row) => {
            if mark.col == start.col {
                Ok(Some(mark))
            } else {
                Err(RelaunchLineError::Wait("typing"))
            }
        }
        Some(_) if unkeyed => Ok(Some(mark)),
        Some(_) => Err(RelaunchLineError::Wait("mark-elsewhere")),
        None if blank_before(row, mark.col) => Ok(Some(mark)),
        None if unkeyed => Err(RelaunchLineError::Wait("unmarked")),
        None => Err(RelaunchLineError::Wait("typing")),
    }
}

fn now_ms() -> u64 {
    aterm_time::unix_millis().unwrap_or(0)
}

struct InputStart {
    row: String,
    col: usize,
}

impl InputStart {
    fn names(&self, cursor_row: &str) -> bool {
        left_of(&self.row, self.col) == left_of(cursor_row, self.col)
    }
}

fn left_of(row: &str, col: usize) -> &str {
    let end = aterm_grapheme::grid_graphemes(row)
        .find(|g| col < g.end_col)
        .map_or(row.len(), |g| g.byte_offset);
    &row[..end]
}

fn first_drawn(row: &str, col: usize) -> Option<usize> {
    aterm_grapheme::grid_graphemes(row)
        .find(|g| g.end_col > col && !g.text.trim().is_empty())
        .map(|g| g.start_col)
}

fn blank_before(row: &str, col: usize) -> bool {
    col > 0
        && aterm_grapheme::grid_graphemes(row)
            .filter(|g| g.start_col < g.end_col && g.start_col < col)
            .last()
            .is_none_or(|g| g.end_col < col || g.text.trim().is_empty())
}

fn input_start(c: &mut Client, tab: &str) -> Option<InputStart> {
    let block = newest_block(c, tab)?;
    if block.get("state").and_then(Value::as_str) != Some("entering") {
        return None;
    }
    let row = block.get("cmd")?.as_u64()?;
    let col = usize::try_from(block.get("cmdcol")?.as_u64()?).ok()?;
    let reply = c.request_line(&format!("@{tab} line {row}")).ok()?;
    let text = reply.strip_prefix("OK")?;
    Some(InputStart {
        row: text.strip_prefix(' ').unwrap_or(text).to_string(),
        col,
    })
}

fn at_mark(mark: &mut Option<PromptMark>, scr: Option<&Screen>) -> Result<(), RelaunchLineError> {
    let Some(scr) = scr else {
        return if mark.is_some() {
            Err(RelaunchLineError::Wait("screen-unreadable"))
        } else {
            Ok(())
        };
    };
    match mark.as_ref().map(|m| m.at(scr)) {
        Some(AtPrompt::Same) => Ok(()),
        Some(AtPrompt::Typed) => Err(RelaunchLineError::Wait("typing")),
        Some(AtPrompt::Moved) => {
            *mark = None;
            Err(RelaunchLineError::Wait("prompt-moved"))
        }
        None if PromptMark::of(scr).is_none() => Ok(()),
        None => Err(RelaunchLineError::Wait("prompt-moved")),
    }
}

/// Where a shell's prompt left the cursor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct PromptMark {
    pub(super) row: usize,
    pub(super) col: usize,
    pub(super) left: String,
    /// The column where right-prompt text began when marked, `None` for a blank rest of row.
    pub(super) right: Option<usize>,
}

/// What a later read of the prompt shows against its [`PromptMark`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AtPrompt {
    /// The cursor where the mark left it (text redrawn in place — a ticking clock — keeps it there), and nothing between it and the right prompt.
    Same,
    /// A person typed after the mark.
    Typed,
    /// Anything else: an async redraw of the prompt, a new prompt below.
    Moved,
}

impl PromptMark {
    pub(super) fn of(scr: &Screen) -> Option<PromptMark> {
        let (row, col) = scr.cursor?;
        let text = scr.rows.get(row)?;
        Some(PromptMark {
            row: scr.first.saturating_add(row),
            col,
            left: left_of(text, col).to_string(),
            right: first_drawn(text, col),
        })
    }

    pub(super) fn at(&self, scr: &Screen) -> AtPrompt {
        let Some(now) = PromptMark::of(scr) else {
            return AtPrompt::Moved;
        };
        if now.row == self.row && now.col == self.col {
            let text = now
                .row
                .checked_sub(scr.first)
                .and_then(|r| scr.rows.get(r))
                .map_or("", String::as_str);
            let end = self.right.unwrap_or(usize::MAX);
            let typed = first_drawn(text, self.col).is_some_and(|at| at < end);
            return if typed {
                AtPrompt::Typed
            } else {
                AtPrompt::Same
            };
        }
        if now.row == self.row && now.col > self.col && now.left.starts_with(&self.left) {
            return AtPrompt::Typed;
        }
        let above = now
            .row
            .checked_sub(scr.first)
            .and_then(|r| r.checked_sub(1))
            .and_then(|r| scr.rows.get(r));
        if above.is_some_and(|row| {
            row.starts_with(&self.left) && row.chars().count() > self.left.chars().count()
        }) {
            return AtPrompt::Typed;
        }
        AtPrompt::Moved
    }

    /// The mark as the state file keeps it.
    pub(super) fn word(&self) -> String {
        format!(
            "v2,{},{},{},{}",
            self.row,
            self.col,
            self.right
                .map_or_else(|| "-".to_string(), |r| r.to_string()),
            self.left
        )
    }

    /// [`Self::word`] read back.
    pub(super) fn parse(word: &str) -> Option<PromptMark> {
        let mut parts = word.strip_prefix("v2,")?.splitn(4, ',');
        Some(PromptMark {
            row: crate::parse_uint(parts.next()?)?,
            col: crate::parse_uint(parts.next()?)?,
            right: match parts.next()? {
                "-" => None,
                r => Some(crate::parse_uint(r)?),
            },
            left: parts.next()?.to_string(),
        })
    }
}

/// How many fenced tries the relaunch line gets ([`type_relaunch_line`]), [`super::Timings::fence_every`] apart, before it is pasted unfenced.
pub(super) const RELAUNCH_FENCE_TRIES: usize = 5;

/// Why aterm's harness watchdog's relaunch line was not typed ([`type_run_line`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunLineRefusal {
    /// The tab was not read as standing at a shell: no socket or roster, no foreground group, or
    /// a foreground process that is no shell (the word says which).
    Unread(&'static str),
    /// Something holds the line for now: a hand, a person's keys, a prompt that moved (the
    /// typer's wait word, or `hand`).
    Held(String),
    /// aterm refused the line's request: the reply.
    Refused(String),
    /// The line's request went unanswered within its wait: it may have run.
    Unanswered,
}

/// THE RELAUNCH LINE OF `words` FOR A `dialect` SHELL: each word quoted for it
/// ([`upgrade::quote`]), joined by one space. The words are the caller's, validated whole, its
/// first the tool.
#[must_use]
pub fn run_line(dialect: Dialect, words: &[String]) -> String {
    words
        .iter()
        .map(|word| upgrade::quote(dialect, word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// THE RELAUNCH LINE OF `words` AS TYPED AT A `dialect` SHELL STANDING IN `shell_cwd`: the
/// words quoted for it ([`run_line`]) where the shell stands in `cwd`, the conversation's
/// directory (or where either is not known), else that line run in a `cd` to `cwd`
/// ([`upgrade::with_cd`]), so the conversation resumes where it ran, never in a directory the
/// shell moved to while the relaunch waited.
///
/// # Errors
/// [`RunLineRefusal::Held`] (`shell-moved`): a shell standing elsewhere whose line cannot carry
/// the `cd` (a fish shell's, a directory with a control character, a line the `cd` makes too
/// long for the terminal) waits for the shell to come back.
pub fn run_line_at(
    dialect: Dialect,
    words: &[String],
    cwd: Option<&str>,
    shell_cwd: Option<&str>,
) -> Result<String, RunLineRefusal> {
    let line = run_line(dialect, words);
    match (cwd, shell_cwd) {
        (Some(want), Some(now)) if !want.is_empty() && !same_dir(Some(now), want) => {
            // A line [`upgrade::with_cd`] reads opens with a space, as every relaunch line does.
            upgrade::with_cd(dialect, &format!(" {line}"), want)
                .map_err(|_| RunLineRefusal::Held("shell-moved".to_string()))
        }
        _ => Ok(line),
    }
}

/// TYPE THE WATCHDOG'S RELAUNCH `words` into tab `tab` on `opts.sock`, at its shell's prompt and
/// under [`Lane::Relaunch`] (the HX-15 plan's D21 and D22): the shell is the process that leads
/// the tab's foreground group once the harness is gone, and its image must be a shell
/// [`Dialect`] names, so nothing is typed into another program in front of the tab. The words
/// are quoted for that shell and run in a `cd` back to `cwd`, the conversation's directory,
/// where the shell stands elsewhere ([`run_line_at`]); no rekey is added.
///
/// # Errors
/// [`RunLineRefusal`]: the tab not read as standing at a shell, a hold on the line (a shell
/// moved where the line cannot follow included), aterm's refusal, or a request that went
/// unanswered.
pub fn type_run_line(
    opts: &Opts,
    tab: &str,
    words: &[String],
    cwd: Option<&str>,
) -> Result<(), RunLineRefusal> {
    let mut c = super::upgrade_drive::connect(opts, tab)
        .map_err(|_| RunLineRefusal::Unread("no-socket"))?;
    let tabs = super::upgrade_drive::host_roster(&mut c).ok_or(RunLineRefusal::Unread("roster"))?;
    let shell = super::relaunch::tab_front(Some(&tabs), tab, NO_SHELL)
        .and_then(|group| u32::try_from(group).ok())
        .ok_or(RunLineRefusal::Unread("no-foreground"))?;
    let dialect = atpkg::caller_shell::process_args(shell)
        .and_then(|args| Dialect::from_exe_name(&args.exec_path))
        .ok_or(RunLineRefusal::Unread("not-a-shell"))?;
    let line = run_line_at(
        dialect,
        words,
        cwd,
        super::upgrade_drive::cwd_of(shell).as_deref(),
    )?;
    let mut mark = None;
    type_relaunch_line_with(
        opts,
        &mut c,
        shell,
        tab,
        &line,
        None,
        PromptLook {
            mark: &mut mark,
            ended_at_s: 0,
            held_since_s: 0,
            line_grace_s: None,
            line_input: None,
        },
        || true,
        |c, pid, tab| super::upgrade_drive::process_in_tab(c, pid, tab, None, None),
        Lane::Relaunch,
    )
    .map_err(|refusal| match refusal {
        RelaunchLineError::Wait(why) => RunLineRefusal::Held(why.to_string()),
        RelaunchLineError::Hand => RunLineRefusal::Held("hand".to_string()),
        RelaunchLineError::Turn(reply) => RunLineRefusal::Refused(reply),
        RelaunchLineError::Unanswered => RunLineRefusal::Unanswered,
    })
}

#[cfg(test)]
#[path = "relaunch_type_tests.rs"]
mod tests;
