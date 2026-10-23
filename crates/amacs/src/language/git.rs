// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Lexing and diagnostics for git commit messages and interactive rebase todo lists.

use super::{
    CommitPhase, Cursor, Diag, DiagKind, LexState, Mode, Severity, Spans, SyntaxClass, diff,
    leading_blanks, line_space,
};

const SCISSORS: &str = "# ------------------------ >8 ------------------------";

const TODO_COMMANDS: &[&str] = &[
    "pick",
    "p",
    "reword",
    "r",
    "edit",
    "e",
    "squash",
    "s",
    "fixup",
    "f",
    "exec",
    "x",
    "break",
    "b",
    "drop",
    "d",
    "label",
    "l",
    "reset",
    "t",
    "merge",
    "m",
    "update-ref",
    "u",
    "noop",
];

fn is_scissors(line: &str) -> bool {
    line.trim_end_matches([' ', '\t', '\r']) == SCISSORS
}

fn is_comment(line: &str) -> bool {
    line.starts_with('#')
}

fn is_blank(line: &str) -> bool {
    line.bytes().all(line_space)
}

pub(super) fn lex_commit(state: LexState, line: &str, out: &mut Spans) -> LexState {
    if state.commit == CommitPhase::Verbose {
        return diff::lex(state, line, out);
    }
    if is_scissors(line) {
        out.push(0, line.len(), SyntaxClass::Comment);
        return LexState {
            mode: Mode::Code,
            commit: CommitPhase::Verbose,
            ..state
        };
    }
    if is_comment(line) {
        out.push(0, line.len(), SyntaxClass::Comment);
        return state;
    }
    match state.commit {
        CommitPhase::Summary if is_blank(line) => state,
        CommitPhase::Summary => {
            out.push(0, line.len(), SyntaxClass::Heading);
            LexState {
                commit: CommitPhase::Body,
                ..state
            }
        }
        _ => {
            trailer(line, out);
            state
        }
    }
}

fn trailer(line: &str, out: &mut Spans) {
    let mut cur = Cursor::new(line);
    let mut hyphens = 0usize;
    cur.eat_while(|byte| {
        if byte == b'-' {
            hyphens = hyphens.saturating_add(1);
        }
        byte.is_ascii_alphanumeric() || byte == b'-'
    });
    if cur.pos() > 0 && hyphens > 0 && cur.eat(b':') && cur.peek().is_none_or(line_space) {
        out.push(0, cur.pos(), SyntaxClass::Keyword);
    }
}

pub(super) fn lex_todo(state: LexState, line: &str, out: &mut Spans) -> LexState {
    let mut cur = Cursor::new(line);
    cur.eat_while(line_space);
    let start = cur.pos();
    if cur.peek() == Some(b'#') {
        cur.finish();
        out.push(start, cur.pos(), SyntaxClass::Comment);
        return state;
    }
    cur.eat_while(|byte| !line_space(byte));
    let command = cur.since(start);
    if !TODO_COMMANDS.contains(&command) {
        return state;
    }
    out.push(start, cur.pos(), SyntaxClass::Keyword);
    let mut argument = word(&mut cur);
    if argument
        .as_ref()
        .is_some_and(|(text, _)| text.starts_with('-'))
    {
        if let Some((_, range)) = argument {
            out.push(range.start, range.end, SyntaxClass::Keyword);
        }
        argument = word(&mut cur);
    }
    let Some((text, range)) = argument else {
        return state;
    };
    let class = match command {
        "exec" | "x" => None,
        "label" | "l" | "reset" | "t" | "update-ref" | "u" => Some(SyntaxClass::Meta),
        _ if text.bytes().all(|byte| byte.is_ascii_hexdigit()) => Some(SyntaxClass::Number),
        _ => None,
    };
    if let Some(class) = class {
        out.push(range.start, range.end, class);
    }
    state
}

fn word<'a>(cur: &mut Cursor<'a>) -> Option<(&'a str, std::ops::Range<usize>)> {
    cur.eat_while(line_space);
    let start = cur.pos();
    if cur.eat_while(|byte| !line_space(byte)) == 0 {
        return None;
    }
    Some((cur.since(start), start..cur.pos()))
}

/// The commit message rules: a summary over `limit` characters, and a line
/// after the summary that is not blank.
pub(super) fn commit_diagnostics(text: &str, limit: usize) -> Vec<Diag> {
    let mut found = Vec::new();
    let mut summary_seen = false;
    let mut offset = 0usize;
    for (index, raw) in text.split('\n').enumerate() {
        let line_start = offset;
        offset = offset.saturating_add(raw.len()).saturating_add(1);
        let line = raw.trim_end_matches([' ', '\t', '\r']);
        if is_scissors(line) {
            break;
        }
        if is_comment(line) {
            continue;
        }
        if summary_seen {
            if !line.is_empty() {
                let start = line_start.saturating_add(leading_blanks(line));
                found.push(Diag {
                    range: start..line_start.saturating_add(line.len()),
                    line: index,
                    severity: Severity::Warning,
                    kind: DiagKind::SecondLineNotBlank,
                    message: "leave the line after the summary blank, so git and its tools \
                              read only the first line as the summary"
                        .to_owned(),
                });
            }
            break;
        }
        if line.trim_start_matches([' ', '\t']).is_empty() {
            continue;
        }
        summary_seen = true;
        let chars = line.chars().count();
        if summary_too_long(chars, limit) {
            let over = line
                .char_indices()
                .nth(limit)
                .map_or(line.len(), |(at, _)| at);
            found.push(Diag {
                range: line_start.saturating_add(over)..line_start.saturating_add(line.len()),
                line: index,
                severity: Severity::Warning,
                kind: DiagKind::SummaryTooLong,
                message: format!("the summary is {chars} characters; keep it to {limit}"),
            });
        }
    }
    found
}

trust_contract_fn! {
    /// Whether a summary of `chars` characters is over the `limit` git's
    /// convention sets: exactly 51 and more for the default 50.
    pub(super) fn summary_too_long(chars: usize, limit: usize) -> bool
    clauses [ensures result == (chars > limit) by summary_too_long_is_past_the_limit]
    {
        chars > limit
    }
}

verify_island! {
    clean {
        theorem summary_too_long_is_past_the_limit (chars limit : UInt64) :
            trust_import_amacs__language__git__summary_too_long chars limit = decide (limit < chars) := by
          simp only [trust_import_amacs__language__git__summary_too_long]
    }
}

/// The rebase todo rule: every line that is not blank or a comment starts
/// with a command git knows.
pub(super) fn todo_diagnostics(text: &str) -> Vec<Diag> {
    let mut found = Vec::new();
    let mut offset = 0usize;
    for (index, line) in text.split('\n').enumerate() {
        let line_start = offset;
        offset = offset.saturating_add(line.len()).saturating_add(1);
        let blanks = leading_blanks(line);
        let rest = line.get(blanks..).unwrap_or("");
        if is_comment(rest) || is_blank(rest) {
            continue;
        }
        let command = rest.split([' ', '\t', '\r']).next().unwrap_or(rest);
        if TODO_COMMANDS.contains(&command) {
            continue;
        }
        let start = line_start.saturating_add(blanks);
        found.push(Diag {
            range: start..start.saturating_add(command.len()),
            line: index,
            severity: Severity::Error,
            kind: DiagKind::UnknownRebaseCommand,
            message: format!(
                "git rebase does not know the command `{command}`; it takes pick, reword, \
                 edit, squash, fixup, exec, break, drop, label, reset, merge, update-ref or noop"
            ),
        });
    }
    found
}
