// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The relaunch line's quoting (HX-15f): each word quoted for the shell that reads it.

use super::*;

/// THE RELAUNCH LINE QUOTES EVERY WORD FOR ITS SHELL: a word with a space, a quote, a `$`, a
/// backslash or a `;` reaches the tool as one argument, as the shell reads it back, and nothing
/// in it is run by the shell.
/// NEGATIVE HALF: the words joined bare would split the space-bearing word and run `; rm`.
#[test]
fn a_relaunch_line_quotes_every_word_for_its_shell() {
    let words: Vec<String> = [
        "claude-alab",
        "run",
        "--",
        "--resume",
        "it's $HOME; rm -rf \\x",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    assert_eq!(
        run_line(Dialect::Zsh, &words),
        r"'claude-alab' 'run' '--' '--resume' 'it'\''s $HOME; rm -rf \x'"
    );
    assert_eq!(
        run_line(Dialect::Bash, &words),
        run_line(Dialect::Zsh, &words)
    );
    assert_eq!(
        run_line(Dialect::Fish, &words),
        r"'claude-alab' 'run' '--' '--resume' 'it\'s $HOME; rm -rf \\x'"
    );
    // NEGATIVE: unquoted, the last word is five words and a command.
    assert_eq!(words.join(" ").split(' ').count(), 9);
}

/// THE RELAUNCH LINE GOES BACK TO THE CONVERSATION'S DIRECTORY WHERE THE SHELL MOVED (HX-15f
/// review): a shell standing where the conversation ran is typed the plain line; one the person
/// moved elsewhere while the relaunch waited is typed that line in a `cd` back, so the
/// conversation resumes in its own directory; a fish shell, whose line carries no `cd`, holds
/// the line until the shell comes back.
/// NEGATIVE HALF: with no directory known, or the shell's directory not read, the plain line.
#[test]
fn a_relaunch_line_goes_back_to_the_conversations_directory() {
    let words: Vec<String> = ["claude-alab", "run", "--", "--resume", "4f1c"]
        .iter()
        .map(ToString::to_string)
        .collect();
    let plain = run_line(Dialect::Zsh, &words);
    let at = |dialect, cwd, shell| run_line_at(dialect, &words, cwd, shell);
    assert_eq!(
        at(Dialect::Zsh, Some("/srv/project"), Some("/srv/project")),
        Ok(plain.clone())
    );
    assert_eq!(
        at(Dialect::Zsh, Some("/srv/project"), Some("/srv/other")),
        Ok(format!(" ( cd -- '/srv/project' && exec {plain} )"))
    );
    assert_eq!(
        at(Dialect::Bash, Some("/srv/project"), Some("/srv/other")),
        Ok(format!(
            " builtin :; ( cd -- '/srv/project' && exec {} )",
            run_line(Dialect::Bash, &words)
        ))
    );
    assert_eq!(
        at(Dialect::Fish, Some("/srv/project"), Some("/srv/other")),
        Err(RunLineRefusal::Held("shell-moved".to_string()))
    );
    // NEGATIVE: nothing to go back to, or no reading of where the shell stands.
    assert_eq!(
        at(Dialect::Zsh, None, Some("/srv/other")),
        Ok(plain.clone())
    );
    assert_eq!(at(Dialect::Zsh, Some("/srv/project"), None), Ok(plain));
}
