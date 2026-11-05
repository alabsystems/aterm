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
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.split('\n').map(str::to_string).collect()
    }

    /// EACH PROGRAM'S COMMAND FORMS, AND THE PROMPTS BESIDE THEM: a slash command, a shell line, a
    /// multi-line draft whose first line with words is one, leading spaces and blank lines before
    /// it, and Claude Code's exit words are commands; a prompt that names a path or a command
    /// mid-line, one opening with `#` or `@`, and an exit word inside a sentence are prompts, as
    /// is, to Claude Code alone, a line opening with a path (its name holds a `/`), which the other
    /// programs refuse as an unknown command. Negative controls by mutation: the first line read
    /// without its blank lines skipped (the blank-led `/clear` reads a prompt); the exit words
    /// matched on the first word (`exit the loop` reads an exit); `!` dropped from the forms (every
    /// shell line reads a prompt); Claude Code's path rule dropped (`/tmp/build.log explain` reads
    /// a slash command); the name read to the line's end, not its first space (`/compact keep
    /// /tmp/notes` reads a prompt).
    #[test]
    fn each_programs_command_forms_are_read_and_its_prompts_are_not() {
        use DraftCommand::{Exit, Shell, Slash, Unread};
        let agents = [
            Program::Claude,
            Program::Codex,
            Program::Agy,
            Program::Muse,
            Program::Grok,
            Program::Kimi,
        ];
        for program in agents {
            for (draft, want) in [
                ("/clear", Some(Slash)),
                ("/exit", Some(Slash)),
                ("/logout", Some(Slash)),
                ("/compact keep the parser notes", Some(Slash)),
                ("/usage", Some(Slash)),
                ("!rm -rf build", Some(Shell)),
                ("!", Some(Shell)),
                ("   /quit", Some(Slash)),
                ("\u{a0}/help", Some(Slash)),
                ("/clear\nthen start on the lexer", Some(Slash)),
                ("\n  \n/clear", Some(Slash)),
                ("keep going on the lexer", None),
                ("fix /tmp/x and run /clear after", None),
                ("# Plan\nship the parser", None),
                ("@src/lib.rs read this", None),
                ("exit the loop when the suite is green", None),
                ("", None),
                ("   ", None),
            ] {
                assert_eq!(
                    draft_command(program, &lines(draft)),
                    want,
                    "{program:?}: {draft:?}"
                );
            }
        }
        for (draft, want) in [
            ("exit", Some(Exit)),
            ("  QUIT  ", Some(Exit)),
            (":q", Some(Exit)),
            (":q!", Some(Exit)),
            (":wq", Some(Exit)),
            (":wq!", Some(Exit)),
            ("exit\nnow", None),
            ("exits", None),
            ("/Users//me/shot.png what is wrong here", None),
            ("/tmp/build.log explain", None),
            ("/src/lib.rs: read this", None),
            ("  //a comment", None),
            ("/tmp what is here", Some(Slash)),
            ("/v1.2 run it", Some(Slash)),
            ("/review:pr 12", Some(Slash)),
            ("/compact keep /tmp/notes", Some(Slash)),
            ("/", Some(Slash)),
        ] {
            assert_eq!(
                draft_command(Program::Claude, &lines(draft)),
                want,
                "Claude Code: {draft:?}"
            );
        }
        for program in [
            Program::Codex,
            Program::Agy,
            Program::Muse,
            Program::Grok,
            Program::Kimi,
        ] {
            assert_eq!(draft_command(program, &lines("exit")), None, "{program:?}");
            assert_eq!(
                draft_command(program, &lines("/tmp/build.log explain")),
                Some(Slash),
                "{program:?}"
            );
        }
        assert_eq!(
            draft_command(Program::Generic, &lines("keep going")),
            Some(Unread)
        );
        assert_eq!(draft_command(Program::Generic, &lines("  ")), None);
    }

    /// EACH PROGRAM'S SHELL MODE AS MEASURED (v6 §14.17.5.8): agy 1.4.12 and Muse Code 1.4.14 take
    /// a typed `!` into their shell mode, whose caret is the `!` itself, so no composer is read and
    /// no draft of theirs opens with `!`; Muse Code takes a pasted `!` there too, while agy keeps a
    /// pasted `!` as text in its `>` composer, which reads as a shell line. Claude Code's own shell
    /// mode draws no composer either (`prompt::fixtures::SHELL_MODE_DRAFT`). Negative control by
    /// mutation: agy's caret row read with `!` as a caret (`agy::is_caret_row`): its bash mode reads
    /// a composer whose draft is no command.
    #[test]
    fn each_programs_shell_mode_as_measured_reads_no_draft_and_a_kept_paste_a_shell_line() {
        use crate::prompt::fixtures::screen;
        for (program, what, fixture) in [
            (
                Program::Agy,
                "agy bash mode",
                crate::agy::fixtures::lab::BASH_MODE_DRAFT,
            ),
            (
                Program::Muse,
                "Muse Code shell mode",
                crate::muse::fixtures::SHELL_MODE_DRAFT,
            ),
            (
                Program::Muse,
                "Muse Code paste",
                crate::muse::fixtures::PASTED_BANG_DRAFT,
            ),
            (
                Program::Claude,
                "Claude Code shell mode",
                crate::prompt::fixtures::SHELL_MODE_DRAFT,
            ),
        ] {
            let rows = screen(fixture);
            assert!(
                rows.iter().any(|r| r.starts_with('!')),
                "{what}: the measured caret row: {rows:#?}"
            );
            assert_eq!(program.reader().composer(&rows), None, "{what}");
        }
        let rows = screen(crate::agy::fixtures::lab::PASTED_BANG_DRAFT);
        let (_, lines) = Program::Agy
            .reader()
            .composer(&rows)
            .expect("agy's composer keeps the paste");
        assert_eq!(
            lines.first().map(String::as_str),
            Some("!echo pasted-probe")
        );
        assert_eq!(
            draft_command(Program::Agy, &lines),
            Some(DraftCommand::Shell)
        );
    }
}
