// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A DRAFT ITS PROGRAM RUNS AS A COMMAND, NOT A PROMPT (v6 §14.17.5.8): what each program's Enter
//! does with the text a composer holds. A person who typed `/clear`, `/logout`, a shell line or
//! an exit word and did not press Enter has not run it; aterm's Enter would run it for them, so a
//! supervisor sends a person's standing draft only where [`draft_command`] reads none.
//!
//! The forms are read from each program's own bundle (the store builds of 2026-10-30):
//!
//! * Claude Code 2.1.316 parses a line opening with `/` by its name, the text after the `/` up to
//!   the first space (its bundle's `YX`). A known name runs. An unknown one is refused as
//!   `Unknown command: /…` where it has a command's form (`^[a-zA-Z0-9_][a-zA-Z0-9:_-]*$`, its
//!   `xin`), or goes to the model under its `cmd_unknown_model_fallback`; a name of any other form
//!   is sent as a prompt (`if(!xin(y))return W()`): a path such as `/tmp/build.log explain`. A
//!   name holding a `/` is a path, never a command (no command name holds one: a folder of
//!   commands is spelled with `:`), so that draft is a prompt here; every other `/` line is read
//!   a command, a dotted name included (a custom command's file name may hold a `.`), as is a
//!   name a file of that path answers (`/tmp what is here`, which Claude Code sends as a
//!   prompt): held as the person's, never sent for them. One opening with `!` is its shell mode
//!   (`! for shell mode`), and the bare words of its exit list
//!   (`["exit","quit",":q",":q!",":wq",":wq!"]`, matched on the trimmed, lowered draft) its own
//!   exit. Its bundle has no `#` memory mode any more (no `memory` input mode, no `# for memory`
//!   hint): a draft opening with `#` is a prompt to it.
//! * agy, Muse Code, codex-alab (`codex`), Grok Build and Kimi Code take `/` as their slash
//!   commands (`/quit`, `/exit` among them) and `!` as their shell mode (agy's `bash mode`, Muse
//!   Code's `Shell mode`, Grok Build's `type ! on an empty prompt`, Kimi Code's `! shell mode`).
//!   A `!` typed into an empty composer takes agy 1.4.12, Muse Code 1.4.14 and Kimi Code 2.4.1
//!   (as Claude Code) into that mode, drawn with `!` as its caret and no composer a reader reads
//!   (measured 2026-11-02: `agy::fixtures::lab::BASH_MODE_DRAFT`,
//!   `muse::fixtures::SHELL_MODE_DRAFT`), so nothing reads a draft there. A composer that opens
//!   with `!` is a `!` line pasted into agy, whose `>` composer keeps a paste as text
//!   (`agy::fixtures::lab::PASTED_BANG_DRAFT`; Muse Code takes a paste into its shell mode too),
//!   or Grok Build's and codex-alab's.
//! * A program no reader names ([`Program::Generic`]) may run any line it is given: no draft of
//!   its reads as a prompt.
//!
//! Leading spaces and blank lines before the first words are no part of the form (each program
//! trims its input before it parses it), and a draft of several lines is read by its first line
//! with words, as each program parses it.

use crate::reader::Program;

/// How a program would take a standing draft on its Enter, where not as a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DraftCommand {
    /// A slash command (`/clear`, `/exit`, `/usage`), or the entry a slash menu's Enter runs.
    Slash,
    /// A line for the program's shell mode (`!ls`).
    Shell,
    /// A bare word the program reads as its own exit (Claude Code's `exit`, `quit`, `:q`).
    Exit,
    /// A line of a program no reader names: whatever it does with it, nothing reads it a prompt.
    Unread,
}

impl DraftCommand {
    /// The form in a few words, for the journal line of the hold it makes (`a person's <words>`).
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::Slash => "slash command",
            Self::Shell => "shell line",
            Self::Exit => "exit word",
            Self::Unread => "line for a program aterm reads no prompts of",
        }
    }
}

/// Claude Code 2.1.316's bare exit words (its bundle's `bX`), matched whole on the trimmed,
/// lowered draft.
pub const CLAUDE_EXIT_WORDS: [&str; 6] = ["exit", "quit", ":q", ":q!", ":wq", ":wq!"];

/// How `program` would take the draft whose composer lines are `lines` on its Enter, where it
/// would not take it as a prompt; `None` for a prompt (and for a draft with no words).
#[must_use]
pub fn draft_command(program: Program, lines: &[String]) -> Option<DraftCommand> {
    let first = lines
        .iter()
        .map(|l| crate::text::trim(l))
        .find(|l| !l.is_empty());
    let opens = |first: Option<&str>| match first?.chars().next()? {
        '/' => Some(DraftCommand::Slash),
        '!' => Some(DraftCommand::Shell),
        _ => None,
    };
    match program {
        Program::Claude => match opens(first) {
            // A name holding a `/` is a path, which Claude Code sends as a prompt.
            Some(DraftCommand::Slash)
                if first
                    .and_then(|l| l.strip_prefix('/'))
                    .and_then(|rest| rest.split(char::is_whitespace).next())
                    .is_some_and(|name| name.contains('/')) =>
            {
                None
            }
            opened => opened,
        }
        .or_else(|| {
            let whole = crate::text::trim(&lines.join("\n")).to_lowercase();
            CLAUDE_EXIT_WORDS
                .contains(&whole.as_str())
                .then_some(DraftCommand::Exit)
        }),
        Program::Codex | Program::Agy | Program::Muse | Program::Grok | Program::Kimi => {
            opens(first)
        }
        Program::Generic => first.map(|_| DraftCommand::Unread),
    }
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
