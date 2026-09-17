// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deterministic block descriptions, and the bounded chrome label composition
//! the tab strip and the window titlebar share.

use super::redaction::contains_sensitive_text;
use super::{ActivityState, Snapshot};
use crate::app_config::TitleFormat;
use std::borrow::Cow;

pub(super) const MAX_DESCRIPTION_GRAPHEMES: usize = 96;
/// Grapheme cap on a title where a chrome surface paints it WHOLE — the window
/// titlebar ([`super::ChromeSurface::WindowTitle`]), and a strip label wherever
/// it leaves the strip for a surface with no fit of its own ([`whole_label`]:
/// the native toolbar's chip and its accessibility string, the tooltip /
/// context-menu header, the a11y tab item, the introspection mirrors) — so a
/// 1024-byte OSC title or a PATH_MAX cwd cannot become an enormous title. The
/// cut is a MIDDLE cut ([`chrome_presentation_text`]), `head…tail`: for a path
/// the tail is the half that says where it ends.
///
/// NOT the tab strip's. The strip fits every label itself, by display cells
/// and SIBLING-AWARE — `tab_bar::distinct_chip_labels` sheds the head and the
/// tail the clustered tabs share and keeps what tells them apart — and a fixed
/// cut taken here first, head or middle, is sibling-blind: it discards one
/// region of every title, so two tabs that differ only inside that region
/// reach the strip as one byte-identical string and paint one label, or
/// ordinals. Measured on glass with sibling directories deeper than the cap
/// (the head cut discards `[96, N)`), and traced again with two worktree
/// checkouts of one repo, `wf_<id>-10` beside `wf_<id>-25` with the id
/// sixty-nine graphemes in, which a middle cut (discarding `[48, N-48)`)
/// collapsed where the head cut had not. So [`super::ChromeSurface::TabStrip`]
/// composes with NO title cap: the strip's inputs are bounded upstream already
/// (`MAX_TITLE_BYTES` for an OSC title, `META_TITLE_MAX` for `meta set title`,
/// `MAX_CWD_PATH_BYTES` for a cwd), and its pairwise prefix/suffix scan over a
/// strip's worth of titles is trivial.
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
/// in its owner; only this native chrome projection is sanitized — and, where
/// the surface paints the title whole, grapheme-capped at `title_cap`, so a
/// 1024-byte authored field cannot become an enormous window title. The strip
/// passes `None`: it fits its labels itself, sibling-aware
/// ([`MAX_CHROME_TITLE_GRAPHEMES`]). The description is capped on every
/// surface — it is a sentence, not a path, and the strip sheds it whole before
/// it cuts a subject (`tab_bar::state_clause_bytes`).
pub(super) fn compose_presentation(
    raw_title: &str,
    description: &str,
    format: TitleFormat,
    separator: &str,
    title_cap: Option<usize>,
) -> String {
    let title = match title_cap {
        Some(max_graphemes) => chrome_presentation_text(raw_title, max_graphemes),
        None => canonical_single_line(raw_title),
    };
    let description = chrome_presentation_text(description, MAX_CHROME_DESCRIPTION_GRAPHEMES);
    compose_parts(&title, &description, format, separator)
}

/// True when the chrome sanitizer and the surface's grapheme cap (`title_cap`,
/// `None` for the strip) pass `title` through byte-identical, so a composition
/// WITHOUT a description may keep it as-is. Printable ASCII with single
/// interior spaces and no edge spaces is exactly identity under
/// `canonical_single_line` (nothing filtered, collapsed, or trimmed), and each
/// such byte is one grapheme, so the cap reduces to `len()`. `compose_parts`
/// then returns the (already-trimmed, non-empty) title verbatim for every
/// format when the description side is empty.
pub(super) fn title_is_presentation_clean(title: &str, title_cap: Option<usize>) -> bool {
    title_cap.is_none_or(|max_graphemes| title.len() <= max_graphemes)
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
pub(super) fn canonical_single_line(text: &str) -> String {
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

/// Sanitize `text` to one canonical line and cap it at `max_graphemes`
/// grapheme clusters, cutting from the MIDDLE: the first `max - max / 2`
/// clusters, one `…`, then the last `max / 2`. At most `max + 1` graphemes
/// leave here, and a text at or under the cap leaves byte-identical — the
/// identity [`title_is_presentation_clean`] relies on. Idempotent in value: a
/// text this cut already shaped is `max + 1` clusters with the mark seated
/// after the head, and cutting it again drops that mark and seats the same
/// one in its place.
///
/// The cut keeps the TAIL because for a path the tail is the half that says
/// where it ends ([`MAX_CHROME_TITLE_GRAPHEMES`]); both cuts land on grapheme
/// boundaries, so a combining mark, a ZWJ sequence or a flag stays whole on
/// either side of the mark. The description takes the same cut: an authored
/// sentence past the cap loses its middle, not its ending.
pub(super) fn chrome_presentation_text(text: &str, max_graphemes: usize) -> String {
    use aterm_grapheme::GraphemeClusters as _;

    let sanitized = canonical_single_line(text);
    // A cluster is at least one byte, so a text within the cap in BYTES is
    // within it in clusters: the ordinary short title never segments.
    if sanitized.len() <= max_graphemes {
        return sanitized;
    }
    let clusters: Vec<&str> = sanitized.graphemes().collect();
    if clusters.len() <= max_graphemes {
        return sanitized;
    }
    let head = max_graphemes - max_graphemes / 2;
    let tail = max_graphemes / 2;
    let mut out = String::with_capacity(sanitized.len());
    out.extend(clusters[..head].iter().copied());
    out.push('…');
    out.extend(clusters[clusters.len() - tail..].iter().copied());
    out
}

/// A composed TAB label on its way to a surface that paints it WHOLE — no cell
/// fit, no sibling-aware pass: the native toolbar's chip and its accessibility
/// string, the tooltip / context-menu header, the a11y tab item, the `tabs` /
/// `chrome` introspection lines. The strip's composition
/// ([`super::ChromeSurface::TabStrip`]) caps no title, so the cap those
/// surfaces always had is applied HERE, half by half on the strip's own seam:
/// the subject and, past [`super::TAB_LABEL_SEPARATOR`], the state clause
/// ([`crate::tab_bar::state_clause_bytes`] — the split the strip itself makes)
/// each take [`MAX_CHROME_TITLE_GRAPHEMES`] by the middle cut. The envelope is
/// exactly what composing with the cap produced (at most `max + 1` graphemes a
/// half), a label whose halves are both within it is borrowed unchanged, and
/// the projection is idempotent in value.
pub(crate) fn whole_label(label: &str) -> Cow<'_, str> {
    use aterm_grapheme::GraphemeClusters as _;

    let within = |half: &str| {
        half.len() <= MAX_CHROME_TITLE_GRAPHEMES
            || half.graphemes().nth(MAX_CHROME_TITLE_GRAPHEMES).is_none()
    };
    let (subject, clause) = label.split_at(label.len() - crate::tab_bar::state_clause_bytes(label));
    // A clause is the separator plus the state, or nothing at all.
    let state = clause
        .strip_prefix(super::TAB_LABEL_SEPARATOR)
        .unwrap_or(clause);
    if within(subject) && within(state) {
        return Cow::Borrowed(label);
    }
    let mut out = chrome_presentation_text(subject, MAX_CHROME_TITLE_GRAPHEMES);
    if !clause.is_empty() {
        out.push_str(super::TAB_LABEL_SEPARATOR);
        out.push_str(&chrome_presentation_text(state, MAX_CHROME_TITLE_GRAPHEMES));
    }
    Cow::Owned(out)
}

/// [`whole_label`] over a strip's worth of labels: borrowed whole when no label
/// is past the cap (the ordinary strip — no allocation), else one owned vector
/// with every label projected.
pub(crate) fn whole_labels(labels: &[String]) -> Cow<'_, [String]> {
    if labels
        .iter()
        .all(|label| matches!(whole_label(label), Cow::Borrowed(_)))
    {
        return Cow::Borrowed(labels);
    }
    Cow::Owned(
        labels
            .iter()
            .map(|label| whole_label(label).into_owned())
            .collect(),
    )
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
