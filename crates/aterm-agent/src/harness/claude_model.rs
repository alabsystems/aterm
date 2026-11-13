// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Claude model as Claude Code writes it: the family and version a model id names, and what
//! the result row of a `/model` or a `/fast on` says about the model.

use super::upgrade::is_model_id;

pub(super) fn base_id(id: &str) -> &str {
    id.strip_suffix("[1m]").unwrap_or(id)
}

/// `claude-<family>-<major>[-<minor>][-<yyyymmdd>]` → family and version.
#[must_use]
pub fn family_version(id: &str) -> Option<(String, (u32, u32))> {
    let base = base_id(id);
    if !is_model_id(base) {
        return None;
    }
    let mut parts = base.strip_prefix("claude-")?.split('-');
    let family = parts.next()?;
    if family.is_empty() || !family.bytes().all(|b| b.is_ascii_lowercase()) {
        return None;
    }
    let nums: Vec<u32> = parts
        .filter(|p| p.len() < 6)
        .map(str::parse::<u32>)
        .collect::<Result<_, _>>()
        .ok()?;
    let major = *nums.first()?;
    let minor = nums.get(1).copied().unwrap_or(0);
    if nums.len() > 2 {
        return None;
    }
    Some((family.to_string(), (major, minor)))
}

/// What one of Claude Code's model result rows says (its `<local-command-stdout>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelResultKind {
    /// `Set model to …`: the model was chosen (`/model`, the picker, a typed `/model <name>`).
    Set,
    /// `Kept model as …`: a `/model` that changed nothing.
    Kept,
    /// `<icon> Fast mode ON · model set to …`: `/fast on` moved the model to one that runs fast.
    FastOn,
}

/// A model result row, read ([`model_result_of`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelResult {
    pub(crate) kind: ModelResultKind,
    /// The model as Claude spelled it, quotes off (`Opus 5.5 (default)`).
    pub(crate) display: Option<String>,
    /// The effort level a `` with `<level>` effort `` clause after it names, as written.
    pub(crate) effort: Option<String>,
}

fn quoted_display(rest: &str) -> Option<(String, &str)> {
    let (display, after) = if let Some(quoted) = rest.strip_prefix('`') {
        let end = quoted.find('`')?;
        (&quoted[..end], &quoted[end + 1..])
    } else {
        let bold = rest.strip_prefix("\u{1b}[1m")?;
        let end = bold.find("\u{1b}[22m")?;
        (&bold[..end], &bold[end + "\u{1b}[22m".len()..])
    };
    Some((display.chars().take(80).collect(), after))
}

fn effort_clause(after: &str) -> Option<String> {
    let (_, rest) = after.split_once(" with `")?;
    let (level, tail) = rest.split_once('`')?;
    tail.starts_with(" effort")
        .then(|| level.chars().take(16).collect())
}

/// `text` without ANSI SGR sequences (`\x1b[1m`, `\x1b[38;2;255;106;0m`), stripped as Claude's result classifier strips them (`/\x1b\[[0-9;]*m/g`).
pub(crate) fn strip_sgr(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('\u{1b}') {
        return text.into();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("\u{1b}[") {
        out.push_str(&rest[..at]);
        let seq = &rest[at + 2..];
        match seq.find(|c: char| !(c.is_ascii_digit() || c == ';')) {
            Some(end) if seq[end..].starts_with('m') => rest = &seq[end + 1..],
            _ => {
                out.push('\u{1b}');
                rest = &rest[at + 1..];
            }
        }
    }
    out.push_str(rest);
    out.into()
}

const FAST_ON_LEAD: usize = 4;

/// What a `type:user` row's content says about the model.
pub(crate) fn model_result_of(content: &str) -> Option<ModelResult> {
    let body = content
        .trim_start()
        .strip_prefix("<local-command-stdout>")?
        .trim_start();
    for (kind, id) in [
        (ModelResultKind::Set, "model.set"),
        (ModelResultKind::Kept, "model.kept"),
    ] {
        if let Some(rest) = body.strip_prefix(aterm_phase::anchor(id)) {
            let quoted = quoted_display(rest);
            return Some(ModelResult {
                kind,
                effort: quoted.as_ref().and_then(|(_, after)| effort_clause(after)),
                display: quoted.map(|(display, _)| display),
            });
        }
    }
    let plain = strip_sgr(body);
    let phrase = format!(
        "{} \u{00B7} {}",
        aterm_phase::anchor!("fast.on"),
        aterm_phase::anchor!("fast.model_set")
    );
    let at = plain
        .find(&phrase)
        .filter(|&at| plain[..at].encode_utf16().count() <= FAST_ON_LEAD)?;
    Some(ModelResult {
        kind: ModelResultKind::FastOn,
        display: quoted_display(&plain[at + phrase.len()..]).map(|(display, _)| display),
        effort: None,
    })
}

#[cfg(test)]
#[path = "claude_model_tests.rs"]
mod tests;
