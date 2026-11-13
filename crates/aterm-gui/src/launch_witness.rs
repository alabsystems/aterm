// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LAUNCH WITNESS: where the person's gesture count stood when the shell signed that a command started — `status launch=<why> launch_seq=<n|->` and `text --json`'s `"launch"`.

use std::sync::{Mutex, PoisonError};

use aterm_core::terminal::{ClockReading, Terminal};

use crate::human_input::HumanInputStamp;

/// The command words a mark can witness: the programs whose folder-trust
/// dialog the harness answers.
const AGENTS: [&str; 2] = ["claude", "codex"];

/// What a mark says ([`LaunchWitness::read`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Why {
    Person,
    Since,
    Typed,
    Boxed,
    Other,
    Pending,
}

impl Why {
    /// The wire word.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Since => "since",
            Self::Typed => "typed",
            Self::Boxed => "boxed",
            Self::Other => "other",
            Self::Pending => "pending",
        }
    }
}

/// The last mark, as a read publishes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Launch {
    /// The engine's started-command count at the mark: one per command line, so a supervisor uses a witness once per id.
    pub(crate) id: u64,
    /// The person count loaded in the mark's hold.
    pub(crate) seq: u64,
    /// The command's first word, when it is one of [`AGENTS`].
    pub(crate) word: Option<&'static str>,
    /// How the mark was judged; `since` is a read's ([`LaunchWitness::read`]).
    pub(crate) why: Why,
}

/// A mark whose screen is still to be read, handed from the hold to [`LaunchWitness::read_screen`] after it.
pub(crate) struct Unread {
    id: u64,
    word: &'static str,
    rows: Vec<String>,
}

/// THE LAST COMMAND'S START, whatever its word ([`LaunchWitness::started`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Started {
    /// The person count loaded in the mark's hold ([`Launch::seq`]).
    pub(crate) seq: u64,
    /// The wall clock the hold took the mark at, unix ms.
    pub(crate) at_ms: u64,
}

/// The last mark and the wall clock its hold took it at, unix ms.
#[derive(Clone, Copy, Debug)]
struct Mark {
    launch: Launch,
    at_ms: u64,
}

/// One session's last mark.
#[derive(Debug, Default)]
pub(crate) struct LaunchWitness {
    mark: Mutex<Option<Mark>>,
}

impl LaunchWitness {
    /// One slice of the program's output through the engine at the reader's `clock`, as the PTY reader's parse stage feeds it, under the terminal lock the caller holds (`t`): the started-command count, the slice, then the mark ([`Self::at_hold`]) in that same hold.
    pub(crate) fn process(
        &self,
        t: &mut Terminal,
        bytes: &[u8],
        clock: ClockReading,
        person: &HumanInputStamp,
    ) -> Option<Unread> {
        let before = t.started_command_seq();
        t.process_at(bytes, clock);
        self.at_hold(t, before, person)
    }

    /// THE MARK, under the terminal lock right after a slice is processed,
    /// `started_before` the engine's started-command count read in the same
    /// hold before the slice.
    fn at_hold(
        &self,
        t: &Terminal,
        started_before: u64,
        person: &HumanInputStamp,
    ) -> Option<Unread> {
        let id = t.started_command_seq();
        if id == started_before || !t.is_require_shell_integration_nonce() {
            return None;
        }
        let probe = person.probe();
        let word = t
            .current_block()
            .and_then(|b| b.commandline.as_deref())
            .and_then(agent_word);
        let why = match word {
            None => Why::Other,
            Some(_) if !probe.plain_enter => Why::Typed,
            Some(_) => Why::Pending,
        };
        let unread = word.filter(|_| why == Why::Pending).map(|word| Unread {
            id,
            word,
            rows: (0..usize::from(t.rows()))
                .map(|r| crate::control::visible_row(t, r))
                .collect(),
        });
        *self.lock() = Some(Mark {
            launch: Launch {
                id,
                seq: probe.seq,
                word,
                why,
            },
            at_ms: now_unix_ms(),
        });
        unread
    }

    /// The mark's screen, read with the terminal lock released: a box the reader of its word finds there (by any cursor: an earlier run's box does not hold it) makes it `boxed`, else `person`.
    pub(crate) fn read_screen(&self, unread: Unread) {
        let Unread { id, word, rows } = unread;
        let read = || {
            let reading = aterm_phase::read(Some(word), &rows, None);
            reading.phase == aterm_phase::Phase::Prompt || reading.prompt.is_some()
        };
        let Some(boxed) =
            crate::reader_guard::read_or_none("launch", "launch witness", &rows, read)
        else {
            return;
        };
        if let Some(mark) = self
            .lock()
            .as_mut()
            .map(|m| &mut m.launch)
            .filter(|m| m.id == id && m.why == Why::Pending)
        {
            mark.why = if boxed { Why::Boxed } else { Why::Person };
        }
    }

    /// The last mark against a read whose person count is `human_seq`: a `person` mark the count has moved past reads `since`.
    pub(crate) fn read(&self, human_seq: u64) -> Option<Launch> {
        let mut launch = (*self.lock())?.launch;
        if launch.why == Why::Person && launch.seq != human_seq {
            launch.why = Why::Since;
        }
        Some(launch)
    }

    /// THE LAST COMMAND'S START, of any word: the person count its hold loaded and the wall clock
    /// it took it at.
    pub(crate) fn started(&self) -> Option<Started> {
        let mark = (*self.lock())?;
        Some(Started {
            seq: mark.launch.seq,
            at_ms: mark.at_ms,
        })
    }

    /// `status`'s `launch=<why|none> launch_seq=<n|->`.
    pub(crate) fn status_fields(&self, human_seq: u64) -> String {
        match self.read(human_seq) {
            Some(l) => format!("launch={} launch_seq={}", l.why.word(), l.seq),
            None => "launch=none launch_seq=-".to_string(),
        }
    }

    /// `text --json`'s `"launch"` value: the whole witness for `person`, the word alone otherwise, `null` for no mark.
    pub(crate) fn json(&self, human_seq: u64) -> String {
        match self.read(human_seq) {
            Some(Launch {
                id,
                seq,
                word: Some(word),
                why: Why::Person,
            }) => format!("{{\"id\":{id},\"seq\":{seq},\"word\":\"{word}\",\"why\":\"person\"}}"),
            Some(l) => format!("{{\"why\":\"{}\"}}", l.why.word()),
            None => "null".to_string(),
        }
    }

    /// A mark as the parse stage would leave it, for a test of a reader of it.
    #[cfg(test)]
    pub(crate) fn mark_for_test(&self, launch: Launch) {
        self.mark_at_for_test(launch, 0);
    }

    /// [`Self::mark_for_test`] taken at `at_ms` (unix ms).
    #[cfg(test)]
    pub(crate) fn mark_at_for_test(&self, launch: Launch, at_ms: u64) {
        *self.lock() = Some(Mark { launch, at_ms });
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Mark>> {
        self.mark.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The wall clock, unix ms: a mark's instant ([`Started::at_ms`]), which is
/// compared with the kernel's start of a process (unix seconds), so it is
/// the wall clock, not this process's monotonic one.
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// The program a command line starts, when it is one of [`AGENTS`]: the
/// basename of its first word exactly, so a wrapper (`exec claude`, `env …`)
/// or an assignment names none.
fn agent_word(line: &str) -> Option<&'static str> {
    let first = line.split_whitespace().next()?;
    let name = first.rsplit('/').next()?;
    AGENTS.into_iter().find(|agent| *agent == name)
}

#[cfg(test)]
#[path = "launch_witness_tests.rs"]
mod tests;
