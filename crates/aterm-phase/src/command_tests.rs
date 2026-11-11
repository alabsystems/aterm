// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
