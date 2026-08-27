// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deterministic block descriptions, and the bounded chrome label composition
//! the tab strip and the window titlebar share.

use super::redaction::contains_sensitive_text;
use super::{ActivityState, Snapshot};
use crate::app_config::TitleFormat;

pub(super) const MAX_DESCRIPTION_GRAPHEMES: usize = 96;
pub(super) const MAX_CHROME_TITLE_GRAPHEMES: usize = 96;
const MAX_CHROME_DESCRIPTION_GRAPHEMES: usize = 96;

pub(super) fn deterministic_description(snapshot: &Snapshot) -> String {
    let place = cwd_label(&snapshot.cwd);
    let place = if contains_sensitive_text(&place) {
        String::new()
    } else {
        place
    };
    let command_is_sensitive =
        !snapshot.command.is_empty() && contains_sensitive_text(&snapshot.command);
    match snapshot.state {
        ActivityState::Prompt => ready_description(&place),
        ActivityState::Entering => {
            if snapshot.command.is_empty() || command_is_sensitive {
                "Typing a command".to_string()
            } else {
                normalize_description(&format!("Typing {}", short_command(&snapshot.command)))
            }
        }
        ActivityState::Executing if command_is_sensitive => "Command running".to_string(),
        ActivityState::Executing => running_description(&snapshot.command),
        ActivityState::Complete if command_is_sensitive => {
            generic_completion_description(snapshot.exit_code)
        }
        ActivityState::Complete => completion_description(&snapshot.command, snapshot.exit_code),
        ActivityState::Unknown => {
            if command_is_sensitive {
                "Command running".to_string()
            } else if !snapshot.command.is_empty() {
                running_description(&snapshot.command)
            } else if !place.is_empty() {
                ready_description(&place)
            } else if !snapshot.title.is_empty() {
                "Active terminal session".to_string()
            } else {
                READY.to_string()
            }
        }
    }
}

/// The description `snapshot` would carry at a settled PROMPT, regardless of its
/// actual block state — the same `Ready in {cwd}` arm (same sensitive-cwd
/// filtering) [`deterministic_description`] uses for [`ActivityState::Prompt`].
/// This is what a stale `Entering` claim decays to once the phase classifier
/// publishes `Idle` (see `Coordinator::note_phase_settled`): the block still
/// says "typing", but nobody has typed for minutes, and the honest label for an
/// abandoned command line is the prompt it is sitting at.
pub(super) fn idle_prompt_description(snapshot: &Snapshot) -> String {
    let place = cwd_label(&snapshot.cwd);
    let place = if contains_sensitive_text(&place) {
        String::new()
    } else {
        place
    };
    ready_description(&place)
}

fn generic_completion_description(exit_code: Option<i32>) -> String {
    if let Some(code) = exit_code.filter(|code| *code != 0) {
        format!("Command failed (exit {code})")
    } else {
        "Command finished".to_string()
    }
}

/// The description of a shell that is simply sitting at its prompt.
///
/// Named because three places must agree on it: the two that produce it, and
/// [`super::shed_place_already_in_title`], which drops it when a tab's title has
/// already said everything it would have said.
pub(super) const READY: &str = "Ready";

fn ready_description(place: &str) -> String {
    if place.is_empty() {
        READY.to_string()
    } else {
        normalize_description(&format!("Ready in {place}"))
    }
}

fn running_description(command: &str) -> String {
    let words = command_words(command);
    let program = words.first().map_or("", String::as_str);
    let sub = words.get(1).map_or("", String::as_str);
    let phrase = match (program, sub) {
        ("cargo", "test") => "Running Rust tests",
        ("cargo", "build") => "Building the project",
        ("cargo", "check") => "Checking the project",
        ("cargo", "clippy") => "Linting Rust code",
        ("cargo", "fmt") => "Formatting Rust code",
        ("cargo", "run") => "Running the project",
        ("git", "pull" | "fetch") => "Updating the repository",
        ("git", "push") => "Publishing commits",
        ("git", "status") => "Inspecting repository status",
        ("git", "diff" | "show" | "log") => "Reviewing repository history",
        ("git", "commit") => "Creating a commit",
        ("git", "merge" | "rebase") => "Integrating repository changes",
        ("npm" | "pnpm" | "yarn", "test") | ("pytest", _) => "Running tests",
        ("npm" | "pnpm" | "yarn", "build") => "Building the project",
        ("npm" | "pnpm" | "yarn", "install" | "add") => "Installing dependencies",
        ("make" | "ninja" | "cmake", _) => "Building the project",
        ("docker", "build") => "Building a container image",
        ("docker", "run" | "compose") => "Running containers",
        ("ssh", _) => "Connected to a remote host",
        ("tail", _) => "Watching live output",
        ("rg" | "grep" | "find", _) => "Searching files",
        ("ls", _) => "Listing files",
        ("python" | "python3" | "node" | "deno" | "bun", _) => "Running a script",
        ("", _) => "Command running",
        _ => return normalize_description(&format!("Running {program}")),
    };
    phrase.to_string()
}

fn completion_description(command: &str, exit_code: Option<i32>) -> String {
    let running = running_description(command);
    if exit_code.is_some_and(|code| code != 0) {
        let subject = running
            .strip_prefix("Running ")
            .or_else(|| running.strip_prefix("Building "))
            .or_else(|| running.strip_prefix("Checking "))
            .unwrap_or(running.as_str());
        return normalize_description(&format!(
            "{} failed (exit {})",
            uppercase_first(subject),
            exit_code.unwrap_or_default()
        ));
    }
    match running.as_str() {
        "Running Rust tests" | "Running tests" => "Tests passed".to_string(),
        "Building the project" => "Build finished".to_string(),
        "Checking the project" => "Project check finished".to_string(),
        "Linting Rust code" => "Lint finished".to_string(),
        "Formatting Rust code" => "Formatting finished".to_string(),
        "Updating the repository" if exit_code == Some(0) => "Repository updated".to_string(),
        "Updating the repository" => "Repository update finished".to_string(),
        "Publishing commits" if exit_code == Some(0) => "Commits published".to_string(),
        "Publishing commits" => "Git push finished".to_string(),
        "Installing dependencies" if exit_code == Some(0) => "Dependencies installed".to_string(),
        "Installing dependencies" => "Dependency command finished".to_string(),
        "Command running" | "Running a command" => "Command finished".to_string(),
        _ => normalize_description(&running.replacen("Running ", "Finished ", 1)),
    }
}

fn command_words(command: &str) -> Vec<String> {
    let mut words: Vec<String> = command
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| matches!(c, '\'' | '"' | ';' | '(' | ')'))
                .to_string()
        })
        .filter(|word| !word.is_empty())
        .collect();
    while words.first().is_some_and(|word| {
        matches!(word.as_str(), "sudo" | "env" | "command" | "time") || word.contains('=')
    }) {
        words.remove(0);
    }
    if let Some(first) = words.first_mut()
        && let Some(base) = first.rsplit('/').next()
    {
        *first = base.to_ascii_lowercase();
    }
    words
}

fn short_command(command: &str) -> String {
    let words = command_words(command);
    let program = words.first().map_or("", String::as_str);
    let sub = words.get(1).map_or("", String::as_str);
    let safe_pair = matches!(
        (program, sub),
        (
            "cargo",
            "test" | "build" | "check" | "clippy" | "fmt" | "run"
        ) | (
            "git",
            "pull"
                | "fetch"
                | "push"
                | "status"
                | "diff"
                | "show"
                | "log"
                | "commit"
                | "merge"
                | "rebase"
        ) | (
            "npm" | "pnpm" | "yarn",
            "test" | "build" | "install" | "add"
        ) | ("docker", "build" | "run" | "compose")
    );
    if safe_pair {
        return format!("{program} {sub}");
    }
    if matches!(
        program,
        "cargo"
            | "git"
            | "npm"
            | "pnpm"
            | "yarn"
            | "make"
            | "ninja"
            | "cmake"
            | "pytest"
            | "python"
            | "python3"
            | "node"
            | "deno"
            | "bun"
            | "docker"
            | "ssh"
            | "tail"
            | "rg"
            | "grep"
            | "find"
            | "ls"
            | "cd"
    ) {
        program.to_string()
    } else {
        "a command".to_string()
    }
}

fn cwd_label(cwd: &str) -> String {
    let trimmed = cwd.trim_end_matches(['/', '\\']);
    trimmed
        .rsplit(['/', '\\'])
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or(trimmed)
        .to_string()
}

fn uppercase_first(value: &str) -> String {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    first.to_uppercase().chain(chars).collect()
}

/// The uncached full composition. Durable OSC/session metadata remains complete
/// in its owner; only this native chrome projection is sanitized and
/// grapheme-capped, preventing a 1024-byte authored field from becoming an
/// enormous tab/window title.
pub(super) fn compose_presentation(
    raw_title: &str,
    description: &str,
    format: TitleFormat,
    separator: &str,
) -> String {
    let title = chrome_presentation_text(raw_title, MAX_CHROME_TITLE_GRAPHEMES);
    let description = chrome_presentation_text(description, MAX_CHROME_DESCRIPTION_GRAPHEMES);
    compose_parts(&title, &description, format, separator)
}

/// True when the chrome sanitizer and grapheme cap pass `title` through
/// byte-identical, so a composition WITHOUT a description may keep it as-is.
/// Printable ASCII with single interior spaces and no edge spaces is exactly
/// identity under `canonical_single_line` (nothing filtered, collapsed, or
/// trimmed), and each such byte is one grapheme, so the cap reduces to `len()`.
/// `compose_parts` then returns the (already-trimmed, non-empty) title verbatim
/// for every format when the description side is empty.
pub(super) fn title_is_presentation_clean(title: &str) -> bool {
    title.len() <= MAX_CHROME_TITLE_GRAPHEMES
        && !title.starts_with(' ')
        && !title.ends_with(' ')
        && !title.contains("  ")
        && title.bytes().all(|byte| (b' '..=b'~').contains(&byte))
}

pub(super) fn compose_parts(
    title: &str,
    description: &str,
    format: TitleFormat,
    separator: &str,
) -> String {
    let title = title.trim();
    let description = description.trim();
    if title.is_empty() && description.is_empty() {
        return "aterm".to_string();
    }
    if title.is_empty() {
        return description.to_string();
    }
    if description.is_empty() || title == description {
        return title.to_string();
    }
    match format {
        TitleFormat::Title => title.to_string(),
        TitleFormat::Description => description.to_string(),
        TitleFormat::TitleDescription => format!("{title}{separator}{description}"),
        TitleFormat::DescriptionTitle => format!("{description}{separator}{title}"),
    }
}

pub(super) fn normalize_description(text: &str) -> String {
    use aterm_grapheme::GraphemeClusters as _;

    canonical_single_line(text)
        .trim_matches([' ', '"', '\''])
        .trim()
        .graphemes()
        .take(MAX_DESCRIPTION_GRAPHEMES)
        .collect()
}

/// Apply the same spoof-resistant presentation policy used for authored session
/// metadata, while preserving the former whitespace-to-one-space behavior expected
/// for terminal/model summaries.
fn canonical_single_line(text: &str) -> String {
    let whitespace_normalized: String = text
        .chars()
        .map(|ch| if ch.is_whitespace() { ' ' } else { ch })
        .collect();
    let filtered = crate::session_timeline::sanitize_presentation_line(
        &whitespace_normalized,
        whitespace_normalized.len(),
    );
    let mut out = String::with_capacity(filtered.len());
    let mut pending_space = false;
    for ch in filtered.chars() {
        if ch == ' ' {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
        }
        pending_space = false;
        out.push(ch);
    }
    out
}

/// Reject polished-looking non-answers that some small local models produce when
/// an idle terminal offers little context. The deterministic summary is more useful
/// than replacing `Ready` with a label that merely restates the feature's purpose.
pub(super) fn is_generic_description(text: &str) -> bool {
    let normalized = text
        .trim_matches(|ch: char| ch.is_whitespace() || ch.is_ascii_punctuation())
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "terminal state"
            | "terminal activity"
            | "terminal state description"
            | "terminal activity description"
            | "terminal state summary"
            | "terminal activity summary"
            | "current terminal state"
            | "current terminal activity"
    )
}

pub(super) fn bounded_text(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|ch| !ch.is_control() && !is_bidi_control(*ch))
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_string()
}

pub(super) fn chrome_presentation_text(text: &str, max_graphemes: usize) -> String {
    use aterm_grapheme::GraphemeClusters as _;

    let sanitized = canonical_single_line(text);
    let mut graphemes = sanitized.graphemes();
    let head: String = graphemes.by_ref().take(max_graphemes).collect();
    if graphemes.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

pub(super) fn is_bidi_control(ch: char) -> bool {
    matches!(
        ch,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}
