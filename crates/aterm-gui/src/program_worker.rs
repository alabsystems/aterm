// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A PROGRAM WORKER (`spawn program=<p> [arg=<a>]… -`): a worker running a program no harness of
//! aterm's reads (design `docs/DESIGN-workers-subagents-2026-11-04.md` §8). Its launcher types the
//! program at the worker's first prompt with the task file as its standard input
//! ([`aterm_agent::harness::worker::program_line`]) and notes on its task record the shell block
//! it typed into ([`crate::task_record::note_program_block`]); the shell's integration (OSC 133)
//! marks the block its command ran in and the status it exited with. Only that block, from the one
//! typed into on, is the program's ([`started`], [`ended`]): a launcher that never typed leaves no
//! block, so nothing a person runs in the tab concludes its task. Its status concludes the task
//! ([`conclude`]): `done why=exit-status exit=0` on 0, `failed why=exit-status exit=<n>` on any
//! other, a signal's as the shell reports it (128 plus its number), and `failed
//! why=no-exit-status` where the program ended with no status read. Its result is its output tail
//! ([`output_tail`]): the last [`TAIL_ROWS`] rows of its block the tab still holds, redacted as the
//! survey's screen reads are ([`crate::survey::redact`]) and cut to [`TAIL_BYTES`], written where
//! `result` reads it ([`aterm_agent::harness::result::snapshot_output`]).
//!
//! The launcher watches its own worker to that end ([`watch`]). A worker whose launcher is not this
//! process's, one a seamless update carried or a restore brought back, is concluded by the
//! reaper's look instead ([`crate::App::reap_workers`], [`unwatched_end`]).

use std::time::Duration;

use aterm_agent::harness::result::{self, DoneBy};
use aterm_core::terminal::{BlockState, OutputBlock, Terminal};

use crate::SessionCtx;
use crate::task_record::{AttentionEdit, MarkFenced, TaskMark, TaskRecord, TaskState};

/// The most rows of the program's output its result keeps: the last ones its block holds.
pub(crate) const TAIL_ROWS: u64 = 200;

/// The most bytes of the program's output its result keeps, the newest kept whole lines.
pub(crate) const TAIL_BYTES: usize = 16 * 1024;

/// The line a result cut at its head opens with.
const CUT: &str = "… earlier output cut";

/// How long the launcher's watch rests with no ring from its session before it reads the task
/// record again ([`watch`]): its program's output and its session's end ring at once.
const WATCH_LOOK: Duration = Duration::from_secs(5);

/// A program worker's program and the words it runs with (`spawn program=<p> [arg=<a>]…`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProgramRun {
    /// The program: a path, or a word its shell finds on its `PATH`.
    pub(crate) program: String,
    /// Its arguments, each one word whatever it holds.
    pub(crate) args: Vec<String>,
}

/// How a program worker's program ended, as its tab's shell reported it ([`ended`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProgramEnd {
    /// Its exit status; `None` where it ended with none read.
    pub(crate) exit: Option<i32>,
    /// Its output tail ([`output_tail`]).
    pub(crate) output: String,
}

impl ProgramEnd {
    /// The state its task concludes in: `done` on status 0, `failed` on any other or none.
    #[must_use]
    pub(crate) const fn state(&self) -> TaskState {
        match self.exit {
            Some(0) => TaskState::Done,
            _ => TaskState::Failed,
        }
    }
}

/// The block the worker's program ran in: the first one from `typed`, the block its launcher typed
/// its line into ([`TaskRecord::program_block`]), whose command ran (OSC 133 `C` marks where its
/// output starts). A shell numbers its blocks upward and runs the line as soon as it is typed, so
/// a command a person runs in the tab after its program is never it; where the line was never
/// typed, its launcher noted no block, and no command is it.
fn program_block(term: &Terminal, typed: u64) -> Option<&OutputBlock> {
    term.all_blocks()
        .find(|block| block.id >= typed && block.output_start_row.is_some())
}

/// Whether `block` ended: the status its shell reported sealed it, or the next prompt did.
fn sealed(block: &OutputBlock) -> bool {
    block.state == BlockState::Complete || block.end_row.is_some()
}

/// Whether `record`'s program has started: its launcher typed its line
/// ([`TaskRecord::program_block`]) and its shell ran it.
#[must_use]
pub(crate) fn started(term: &Terminal, record: &TaskRecord) -> bool {
    record
        .program_block
        .is_some_and(|typed| program_block(term, typed).is_some())
}

/// How the program its launcher typed at block `typed` ended, read off `term`; `None` while it
/// runs, before it started, or while job control holds it. Its block ends with the status its
/// shell reported (OSC 133 `D`), or with none where the next prompt sealed it first. A status that
/// says job control stopped it ([`stop_status`]) is no end while `stopped` says its shell holds a
/// stopped job, nor while a later command runs (the `fg` that resumed it); once neither holds, the
/// program ended where job control hid how, so its end has no status read, unless no command ran
/// after it, where it is the program's own status.
#[must_use]
pub(crate) fn ended(
    term: &Terminal,
    typed: u64,
    stopped: impl FnOnce() -> bool,
) -> Option<ProgramEnd> {
    let block = program_block(term, typed)?;
    let mut exit = match block.state {
        BlockState::Complete => block.exit_code,
        _ if block.end_row.is_some() => None,
        _ => return None,
    };
    if exit.is_some_and(stop_status) {
        if stopped() {
            return None;
        }
        let mut later = term
            .all_blocks()
            .filter(|later| later.id > block.id && later.output_start_row.is_some())
            .peekable();
        if later.peek().is_some() {
            if later.any(|later| !sealed(later)) {
                return None;
            }
            exit = None;
        }
    }
    Some(ProgramEnd {
        exit,
        output: output_tail(term, block),
    })
}

/// Whether `status` is how zsh and bash report a job that job control stopped: 128 plus
/// `SIGTSTP`, `SIGSTOP`, `SIGTTIN` or `SIGTTOU`. A program may exit with that status itself, which
/// only [`ended`]'s look at its shell tells apart.
fn stop_status(status: i32) -> bool {
    #[cfg(unix)]
    {
        [libc::SIGTSTP, libc::SIGSTOP, libc::SIGTTIN, libc::SIGTTOU]
            .into_iter()
            .any(|signal| status == 128 + signal)
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        false
    }
}

/// Whether the shell `pid` holds a job that job control stopped: a child of it the kernel reports
/// stopped. Read only where a program's block ended with a stop's status ([`ended`]).
#[must_use]
pub(crate) fn holds_stopped_job(pid: i32) -> bool {
    children_of(pid).is_some_and(|children| {
        children
            .into_iter()
            .filter_map(|child| u32::try_from(child).ok())
            .any(aterm_uds::process::is_stopped)
    })
}

/// The live children of `pid` the kernel lists; `None` where it lists none here.
fn children_of(pid: i32) -> Option<Vec<i32>> {
    #[cfg(target_vendor = "apple")]
    {
        aterm_uds::process::child_pids(pid)
    }
    #[cfg(target_os = "linux")]
    {
        crate::foreground_handback::linux_children(pid)
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
    {
        let _ = pid;
        None
    }
}

/// The tail of `block`'s output in `term`: its last [`TAIL_ROWS`] rows that scrollback still
/// holds (to the screen's last row while the next prompt has not sealed it), redacted
/// ([`crate::survey::redact`]) and cut to its last [`TAIL_BYTES`] at a line's start, opening with
/// [`CUT`] where anything earlier was left out.
#[must_use]
pub(crate) fn output_tail(term: &Terminal, block: &OutputBlock) -> String {
    let Some(start) = block.output_start_row else {
        return String::new();
    };
    let grid = term.grid();
    let oldest = grid.oldest_absolute_row();
    let screen_end = oldest
        .saturating_add(grid.scrollback_lines() as u64)
        .saturating_add(u64::from(grid.rows()));
    let end = block.end_row.unwrap_or(screen_end).min(screen_end);
    let from = start.max(end.saturating_sub(TAIL_ROWS)).max(oldest);
    let mut tail = block.clone();
    tail.output_start_row = Some(from);
    tail.end_row = Some(end.max(from));
    let text = term.block_output(&tail).unwrap_or_default();
    cut_to(&crate::survey::redact(&text), TAIL_BYTES, from > start)
}

/// `text`'s last `max` bytes from the start of a line (from a character's where one line is
/// longer), opening with [`CUT`] where anything was left out, `cut` included.
fn cut_to(text: &str, max: usize, cut: bool) -> String {
    let text = text.trim_end();
    let kept = if text.len() <= max {
        text
    } else {
        let mut at = text.len() - max;
        while !text.is_char_boundary(at) {
            at += 1;
        }
        let rest = &text[at..];
        let starts_a_line = text.as_bytes().get(at - 1) == Some(&b'\n');
        match rest.split_once('\n') {
            Some((_, whole)) if !starts_a_line && !whole.is_empty() => whole,
            _ => rest,
        }
    };
    if kept.len() < text.len() || cut {
        format!("{CUT}\n{kept}")
    } else {
        kept.to_string()
    }
}

/// Whether `record` is a program worker's task its program's end may still conclude: a program
/// worker's ([`TaskRecord::runs_a_program`]), `working` or `stalled` (by a deadline that read it
/// before its program started), whose launcher typed its line ([`TaskRecord::program_block`]), or
/// one a restore brought back (`why=restored`), whose program ran, if at all, in a shell that is
/// gone. A launcher that gave up before typing leaves its task to its stall.
#[must_use]
pub(crate) fn awaits_its_end(record: &TaskRecord) -> bool {
    record.runs_a_program()
        && matches!(record.state, TaskState::Working | TaskState::Stalled)
        && (record.program_block.is_some() || record.why == Some(crate::task_record::WHY_RESTORED))
}

/// Concludes session `local`'s task (sid `sid`) on `end`, its record as `read` saw it: writes its
/// result, then marks it `end`'s state, `why=exit-status` with its `exit=` where a status was read
/// and `why=no-exit-status` where none was, and the result, if the
/// record still stands as read ([`awaits_its_end`], same rev), waiting at most `fence_wait` on a committing
/// update's fence. The mark takes down a deadline's attention, which a program that ended no
/// longer owes. `Ok(None)` where the record moved first: its snapshot stays, the newer mark's own
/// pruning removes it.
///
/// # Errors
/// [`MarkFenced`]: an update was committing past `fence_wait`.
pub(crate) fn conclude(
    ctx: &SessionCtx,
    local: u64,
    sid: &str,
    read: &TaskRecord,
    end: &ProgramEnd,
    fence_wait: Duration,
) -> Result<Option<TaskRecord>, MarkFenced> {
    let state = end.state();
    let mut mark = TaskMark::new(state);
    mark.why = Some(if end.exit.is_some() {
        crate::task_record::WHY_EXIT_STATUS
    } else {
        crate::task_record::WHY_NO_EXIT_STATUS
    });
    mark.exit = end.exit;
    let stem = result::result_stem(Some(sid), read.rev);
    let by = if state == TaskState::Done {
        DoneBy::Turn
    } else {
        DoneBy::Failed
    };
    let written = crate::task_record::tasks_dir()
        .ok_or_else(|| "no state folder".to_string())
        .and_then(|tasks| {
            result::snapshot_output(&tasks, &stem, &end.output, by)
                .map(|snap| (tasks, snap))
                .map_err(|e| e.to_string())
        });
    match &written {
        Ok((_, snap)) => {
            mark.result_bytes = snap.bytes;
            mark.result_path = Some(snap.path.clone());
        }
        Err(why) => aterm_log::warn!("worker session {local}: its result was not written: {why}"),
    }
    let marked = crate::task_record::mark_session_task_with(
        ctx,
        local,
        Some(&mark),
        AttentionEdit::TakeDown(crate::harness_host::deadline_attention_stands),
        fence_wait,
        |record, _| record.is_some_and(|record| record.rev == read.rev && awaits_its_end(record)),
    )?;
    let Some(marked) = marked else {
        return Ok(None);
    };
    if marked.attention_moved {
        crate::presence::post_meta_changed(local);
    }
    if let Ok((tasks, _)) = &written {
        result::prune_results(tasks, &stem);
    }
    let exit = end
        .exit
        .map_or_else(|| "with no status read".to_string(), |n| format!("{n}"));
    aterm_log::info!(
        "worker session {local}: its program exited {exit}; its task is {}",
        state.as_str()
    );
    Ok(marked.record)
}

/// Watches the program worker `work`'s launcher typed its line into until its program ends, then
/// concludes its task ([`conclude`]). It reads the screen at each ring of its session (output, its
/// end) and the record at least every [`WATCH_LOOK`], and stops where its session ended or its
/// task is no longer the one it typed for, still [`awaits_its_end`] with its typed block noted: whatever
/// moved it said how it ended.
pub(crate) fn watch(work: &crate::task_launch::Launch) {
    let sub = crate::session_watch::Subscription::single(&work.ctx.watchers);
    let started = work
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .task()
        .map(|task| task.started);
    loop {
        if !crate::task_launch::session_alive(work) {
            return;
        }
        let read = work
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .task()
            .filter(|task| Some(task.started) == started && awaits_its_end(task))
            .cloned();
        let Some((read, typed)) =
            read.and_then(|read| read.program_block.map(|typed| (read, typed)))
        else {
            return;
        };
        let shell = shell_pid(work);
        let end = ended(&crate::term_lock(&work.term), typed, || {
            shell.is_some_and(holds_stopped_job)
        });
        if let Some(end) = end {
            let sid = work.ctx.self_id.as_str().to_string();
            match conclude(
                &work.ctx,
                work.local,
                &sid,
                &read,
                &end,
                work.ctx.fabric.fence_wait(),
            ) {
                Ok(Some(_)) => return,
                // Moved since the read: the next turn reads what moved it.
                Ok(None) => continue,
                Err(MarkFenced) => {}
            }
        }
        let _ = sub.wait(WATCH_LOOK);
    }
}

/// The pid of the shell holding `work`'s session, `None` once it is gone.
fn shell_pid(work: &crate::task_launch::Launch) -> Option<i32> {
    work.store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .by_local(work.local)
        .map(|h| h.shell_identity.pid)
        .filter(|pid| *pid > 0)
}

/// What a look that is not the launcher's reads of a program worker's tab ([`unwatched_end`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Unwatched {
    /// The task record says a restore other than a seamless update brought it back
    /// (`why=restored`): its program ran in a shell that is gone.
    pub(crate) restored: bool,
    /// A foreground job runs in its tab.
    pub(crate) job: bool,
}

/// How a program worker's program ended, for a look that is not its launcher's (the reaper's,
/// [`crate::App::reap_workers`]), its record `record` ([`awaits_its_end`]): for a worker a restore brought
/// back, an end with no status read once no job runs in front, since its program ran in a shell
/// that is gone and no block of this one is its; else the end its tab's blocks hold from the one
/// its launcher typed into ([`ended`], `stopped` as there); else `None`, as it may still run.
#[must_use]
pub(crate) fn unwatched_end(
    term: &Terminal,
    record: &TaskRecord,
    stand: Unwatched,
    stopped: impl FnOnce() -> bool,
) -> Option<ProgramEnd> {
    if stand.restored {
        return (!stand.job).then(|| ProgramEnd {
            exit: None,
            output: String::new(),
        });
    }
    ended(term, record.program_block?, stopped)
}

#[cfg(test)]
#[path = "program_worker_tests.rs"]
mod tests;
