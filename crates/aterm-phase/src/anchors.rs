// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The literal strings a supervisor guards on, in ONE table.
//!
//! A guard (`key if=<regex> 1`, `await match`, `turn … submit=guarded:<re>`) is only as
//! good as the text it names, and that text is Claude Code's, not ours: the
//! day a release renames a header or a hint, a guard spelled inline in some
//! other crate silently stops matching. So the strings live here, each with
//! an id a caller imports ([`anchor`]) instead of re-typing the words, the
//! release they were read on, and whether they are one literal in the
//! vendor's binary or composed at run time from key hints.
//!
//! The ignored test `every_literal_anchor_is_in_the_store_claude` greps the
//! store's `claude` binary for every [`AnchorKind::Literal`] row and prints
//! the misses — the drift canary to run when the pinned build moves.

/// How an anchor's text reaches the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorKind {
    /// One string in the vendor's binary (a `·` in it is drawn from a
    /// `\xB7` escape, so the canary looks for the parts either side of it).
    Literal,
    /// Composed at run time from the user's key bindings (`Esc to cancel` is
    /// the `confirm:no` binding's key plus a description): no single literal
    /// in the binary carries it, and a rebinding changes the key word.
    KeyHint,
    /// Assembled at run time from parts (`Do you want to ` + `make this edit
    /// to` + the file name; the rm breaker's warning from a template): the
    /// canary cannot look for it whole.
    Composed,
}

/// One guarded string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    /// The id callers import it by (`prompt.proceed`).
    pub id: &'static str,
    /// The text as the screen shows it.
    pub text: &'static str,
    pub kind: AnchorKind,
    /// The Claude Code release it was read on (a capture or the binary).
    pub since: &'static str,
}

const fn lit(id: &'static str, text: &'static str, since: &'static str) -> Anchor {
    Anchor {
        id,
        text,
        kind: AnchorKind::Literal,
        since,
    }
}

const fn composed(id: &'static str, text: &'static str, since: &'static str) -> Anchor {
    Anchor {
        id,
        text,
        kind: AnchorKind::Composed,
        since,
    }
}

const fn hint(id: &'static str, text: &'static str, since: &'static str) -> Anchor {
    Anchor {
        id,
        text,
        kind: AnchorKind::KeyHint,
        since,
    }
}

/// Every string aterm guards on in Claude Code's screen.
pub const ANCHORS: &[Anchor] = &[
    // The approval box.
    lit("prompt.proceed", "Do you want to proceed?", "2.1.267"),
    composed("prompt.edit", "Do you want to make this edit to", "2.1.280"),
    hint("prompt.cancel", "Esc to cancel", "2.1.267"),
    hint("prompt.amend", "Tab to amend", "2.1.267"),
    hint("prompt.confirm", "Enter to confirm", "2.1.280"),
    lit("box.bash", "Bash command", "2.1.267"),
    lit("box.edit", "Edit file", "2.1.267"),
    lit("box.write", "Write file", "2.1.267"),
    lit("box.create", "Create file", "2.1.267"),
    lit("box.overwrite", "Overwrite file", "2.1.280"),
    lit("box.read", "Read file", "2.1.267"),
    lit("box.workflow", "Run a dynamic workflow?", "2.1.267"),
    composed(
        "box.rm_breaker",
        "Dangerous rm operation on possibly-empty variable path",
        "2.1.278",
    ),
    // 2.1.281: a box in a session read as unattended carries a countdown
    // row under its notes (`⚠ Claude Code will automatically deny this
    // request in 1:59, to avoid blocking progress on an unattended
    // session`), the time ticking — its own row, never a note.
    composed(
        "box.auto_deny",
        "Claude Code will automatically deny this request in",
        "2.1.281",
    ),
    // The folder-trust dialog.
    lit("trust.title", "Accessing workspace:", "2.1.280"),
    lit("trust.yes", "Yes, I trust this folder", "2.1.280"),
    lit("trust.no", "No, exit", "2.1.280"),
    // The live zone.
    lit("busy.interrupt", "esc to interrupt", "2.1.267"),
    // A turn a person stopped with Esc: the vendor's row under the last
    // message or tool row (`  ⎿  Interrupted · What should Claude do
    // instead?`, measured 2026-09-23 on the lane probe's worker).
    lit(
        "turn.interrupted",
        "Interrupted · What should Claude do instead?",
        "2.1.280",
    ),
    lit(
        "survey.question",
        "How is Claude doing this session",
        "2.1.267",
    ),
    lit("context.indicator", "until auto-compact", "2.1.267"),
    lit("goal.active", "/goal active", "2.1.278"),
    lit(
        "notice.update",
        "Update installed · Restart to update",
        "2.1.267",
    ),
    lit(
        "model.saved",
        "saved as your default for new sessions",
        "2.1.267",
    ),
    // The walls (see `wall::PHRASES` for the whole classification table).
    lit("wall.fable", "You've reached your Fable limit", "2.1.267"),
    lit("wall.auto_continue", "continuing automatically", "2.1.268"),
    lit("wall.context", "Context limit reached", "2.1.280"),
    lit("wall.server", "This is a server-side issue", "2.1.280"),
    lit(
        "wall.overloaded",
        "Repeated 529 Overloaded errors",
        "2.1.280",
    ),
    lit("wall.login", "Please run /login", "2.1.280"),
    lit("wall.goal_paused", "Goal paused", "2.1.280"),
];

/// The text of the anchor `id`. Panics on an id the table does not have —
/// a typo in a caller is a bug to find in its first test, not a guard that
/// silently matches nothing.
#[must_use]
pub fn anchor(id: &str) -> &'static str {
    match ANCHORS.iter().find(|a| a.id == id) {
        Some(a) => a.text,
        None => panic!("aterm-phase has no anchor {id:?}"),
    }
}

/// [`anchor`] at COMPILE time, for a caller that needs the text in a
/// `const` (`const NOTE: &str = anchor_text("box.rm_breaker");`): an id the
/// table does not have fails the build, not the first test.
#[must_use]
pub const fn anchor_text(id: &str) -> &'static str {
    const fn same(a: &str, b: &str) -> bool {
        let (a, b) = (a.as_bytes(), b.as_bytes());
        if a.len() != b.len() {
            return false;
        }
        let mut i = 0;
        while i < a.len() {
            if a[i] != b[i] {
                return false;
            }
            i += 1;
        }
        true
    }
    let mut i = 0;
    while i < ANCHORS.len() {
        if same(ANCHORS[i].id, id) {
            return ANCHORS[i].text;
        }
        i += 1;
    }
    panic!("aterm-phase has no anchor with that id")
}

/// The first `words` words of anchor `id` as a guard regex
/// ([`guard_regex`]): a fence that must still match when the vendor's row
/// runs on past them (`How.is.Claude.doing` over the whole survey question).
#[must_use]
pub fn guard_prefix(id: &str, words: usize) -> String {
    let text = anchor(id);
    let cut = text.split(' ').take(words).map(str::len).sum::<usize>() + words.saturating_sub(1);
    guard_regex(&text[..cut.min(text.len())])
}

/// `text` as a regex that matches it literally in a `key if=` / `await
/// match` guard: every regex metacharacter escaped, and each space spelled
/// `.` because the control verbs split their arguments on whitespace.
#[must_use]
pub fn guard_regex(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        match c {
            ' ' => out.push('.'),
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_every_text_is_non_empty() {
        for (i, a) in ANCHORS.iter().enumerate() {
            assert!(!a.text.trim().is_empty(), "{}", a.id);
            assert!(
                ANCHORS[i + 1..].iter().all(|b| b.id != a.id),
                "duplicate id {}",
                a.id
            );
        }
        assert_eq!(anchor("prompt.proceed"), "Do you want to proceed?");
        // The compile-time accessor reads the same table.
        const PROCEED: &str = anchor_text("prompt.proceed");
        assert_eq!(PROCEED, anchor("prompt.proceed"));
        for a in ANCHORS {
            assert_eq!(anchor_text(a.id), a.text, "{}", a.id);
        }
    }

    #[test]
    fn a_guard_prefix_is_the_leading_words_as_a_guard() {
        assert_eq!(guard_prefix("survey.question", 4), "How.is.Claude.doing");
        assert_eq!(guard_prefix("busy.interrupt", 3), "esc.to.interrupt");
        // More words than the anchor has is the whole anchor, never a panic.
        assert_eq!(guard_prefix("busy.interrupt", 9), "esc.to.interrupt");
        assert_eq!(
            guard_prefix("prompt.proceed", 5),
            "Do.you.want.to.proceed\\?"
        );
    }

    #[test]
    #[should_panic(expected = "no anchor")]
    fn an_unknown_id_panics() {
        let _ = anchor("prompt.nope");
    }

    #[test]
    fn a_guard_regex_escapes_metacharacters_and_dots_its_spaces() {
        assert_eq!(
            guard_regex("Do you want to proceed?"),
            "Do.you.want.to.proceed\\?"
        );
        assert_eq!(
            guard_regex("Run a (dynamic) x.y"),
            "Run.a.\\(dynamic\\).x\\.y"
        );
    }

    /// The drift canary (run by hand when the pinned `claude` moves:
    /// `targo --unverified test -p aterm-phase -- --ignored --nocapture`).
    /// Reads `$ATERM_CLAUDE_BIN`, else asks `aterm pkg which claude` for the
    /// store path, and FAILS naming every literal anchor the binary no longer
    /// has — and when it finds no binary, since a canary that cannot look
    /// has not passed. A `·` in an anchor is drawn from a `\xB7` escape, so
    /// each side of it is looked for on its own.
    #[test]
    #[ignore = "reads the installed claude binary"]
    fn every_literal_anchor_is_in_the_store_claude() {
        let path = std::env::var("ATERM_CLAUDE_BIN").ok().or_else(|| {
            let out = std::process::Command::new("aterm")
                .args(["pkg", "which", "claude"])
                .output()
                .ok()?;
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            // `claude → <shim> → <store path> — managed …`
            text.lines()
                .next()?
                .split(" → ")
                .nth(2)
                .map(|s| s.split(" — ").next().unwrap_or(s).trim().to_string())
        });
        let Some(path) = path else {
            panic!("no claude binary found: set ATERM_CLAUDE_BIN");
        };
        let bytes = std::fs::read(&path).expect("read the claude binary");
        let has = |needle: &str| bytes.windows(needle.len()).any(|w| w == needle.as_bytes());
        let mut missing = Vec::new();
        for a in ANCHORS.iter().filter(|a| a.kind == AnchorKind::Literal) {
            if !a.text.split(" · ").all(has) {
                missing.push(a.id);
            }
        }
        assert!(
            missing.is_empty(),
            "{path}: {} literal anchors missing: {missing:?}",
            missing.len()
        );
        eprintln!("{path}: every literal anchor is present");
    }
}
