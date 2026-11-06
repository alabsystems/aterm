// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A `repo =` SOURCE'S REMOTE, READ OUT OF ITS `.git/config` IN PROCESS (the slice-3 plan's §0 Q3
//! limit 6 and S4): no git process ever runs in the owner's repository, so its hooks, its
//! fsmonitor and its config's commands never run either.
//!
//! [`remote_url`] is a minimal git-config reader for one key, `[remote "<name>"] url`, over the
//! file's bytes. It refuses rather than guess: any `[include]` or `[includeIf …]` section (an
//! include names another file, which would be a second read the owner never pointed aterm at), a
//! continuation line, a line it cannot read, more than one `url` for the remote, and a file over
//! [`CONFIG_MAX`]. The URL it finds goes through [`RemoteUrl::parse`], so a token in it refuses
//! the source and no raw URL leaves this module. Opening the file (no-follow, a regular file) is
//! the reader's.

use super::git_url::{RemoteUrl, UrlRefusal};

/// The most bytes of a `.git/config` read.
pub const CONFIG_MAX: usize = 64 * 1024;

/// Why a repository's remote URL is not read (`unread why=repo-config`, or the URL's own word).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoConfigRefusal {
    /// An `[include]` or `[includeIf …]` section.
    Include,
    /// A line ending in a backslash, continued on the next.
    Continuation,
    /// Over [`CONFIG_MAX`] bytes.
    Long,
    /// Not UTF-8, or a line that is no section, assignment, comment or blank.
    Unreadable,
    /// No `[remote "<name>"]` section names a `url`.
    NoUrl,
    /// The remote names more than one `url`.
    Several,
    /// The URL is one [`RemoteUrl::parse`] refuses.
    Url(UrlRefusal),
}

impl RepoConfigRefusal {
    /// Its word: `repo-config` for the file's own shape, the URL's word for a URL refused, so a
    /// token in the remote reads `userinfo` as it would in `url =`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            RepoConfigRefusal::Url(why) => why.word(),
            RepoConfigRefusal::NoUrl => "no-remote",
            RepoConfigRefusal::Include
            | RepoConfigRefusal::Continuation
            | RepoConfigRefusal::Long
            | RepoConfigRefusal::Unreadable
            | RepoConfigRefusal::Several => "repo-config",
        }
    }
}

/// THE URL OF REMOTE `remote` IN A `.git/config` OF `config` BYTES ([module docs](self)).
/// # Errors
/// [`RepoConfigRefusal`]: the first reason it is not read.
pub fn remote_url(config: &[u8], remote: &str) -> Result<RemoteUrl, RepoConfigRefusal> {
    use super::fault::{Fault, armed};
    if config.len() > CONFIG_MAX {
        return Err(RepoConfigRefusal::Long);
    }
    let text = std::str::from_utf8(config).map_err(|_| RepoConfigRefusal::Unreadable)?;
    let mut in_remote = false;
    let mut url: Option<String> = None;
    for line in text.lines() {
        let line = line.trim_start_matches('\u{feff}').trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[') {
            let section = section(header)?;
            if section.include && !armed(Fault::IncludeFollowed) {
                return Err(RepoConfigRefusal::Include);
            }
            in_remote = section.name == "remote" && section.sub.as_deref() == Some(remote);
            continue;
        }
        let (key, value) = assignment(line)?;
        if in_remote && key.eq_ignore_ascii_case("url") {
            if url.is_some() {
                return Err(RepoConfigRefusal::Several);
            }
            url = Some(value);
        }
    }
    let url = url.ok_or(RepoConfigRefusal::NoUrl)?;
    RemoteUrl::parse(&url).map_err(RepoConfigRefusal::Url)
}

struct Section {
    /// Lower case.
    name: String,
    /// Exact, as its quotes hold it.
    sub: Option<String>,
    include: bool,
}

/// A header after its `[`: `name]`, `name "sub"]`, or `name.sub]`, then at most a comment.
fn section(header: &str) -> Result<Section, RepoConfigRefusal> {
    let (inside, after) = split_header(header)?;
    let after = after.trim();
    if !(after.is_empty() || after.starts_with('#') || after.starts_with(';')) {
        return Err(RepoConfigRefusal::Unreadable);
    }
    let (name, sub) = match inside.split_once(char::is_whitespace) {
        Some((name, quoted)) => (name, Some(unquote_sub(quoted.trim())?)),
        None => (inside, None),
    };
    let lower = name.to_ascii_lowercase();
    let head = lower.split('.').next().unwrap_or_default();
    let include = head == "include" || head == "includeif";
    let shaped = !lower.is_empty()
        && lower
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.');
    if !shaped {
        return Err(RepoConfigRefusal::Unreadable);
    }
    Ok(Section {
        name: lower,
        sub,
        include,
    })
}

/// The header's inside and what follows its `]`, minding a quoted subsection's `\"` and `]`.
fn split_header(header: &str) -> Result<(&str, &str), RepoConfigRefusal> {
    let mut quoted = false;
    let mut escaped = false;
    for (at, c) in header.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ']' if !quoted => return Ok((&header[..at], &header[at + 1..])),
            _ => {}
        }
    }
    Err(RepoConfigRefusal::Unreadable)
}

fn unquote_sub(quoted: &str) -> Result<String, RepoConfigRefusal> {
    let inner = quoted
        .strip_prefix('"')
        .and_then(|q| q.strip_suffix('"'))
        .ok_or(RepoConfigRefusal::Unreadable)?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next().ok_or(RepoConfigRefusal::Unreadable)?),
            '"' => return Err(RepoConfigRefusal::Unreadable),
            c => out.push(c),
        }
    }
    Ok(out)
}

/// `key = value` or a bare `key` (git's true), its value unquoted and unescaped, a trailing
/// comment dropped.
fn assignment(line: &str) -> Result<(&str, String), RepoConfigRefusal> {
    let (key, raw) = match line.split_once('=') {
        Some((key, raw)) => (key.trim(), raw.trim_start()),
        None => (line, ""),
    };
    let shaped = key.bytes().next().is_some_and(|b| b.is_ascii_alphabetic())
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !shaped {
        return Err(RepoConfigRefusal::Unreadable);
    }
    Ok((key, value(raw)?))
}

fn value(raw: &str) -> Result<String, RepoConfigRefusal> {
    let mut out = String::with_capacity(raw.len());
    let mut quoted = false;
    let mut kept = 0;
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                None => return Err(RepoConfigRefusal::Continuation),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('b') => {
                    out.pop();
                }
                Some(c @ ('"' | '\\')) => out.push(c),
                Some(_) => return Err(RepoConfigRefusal::Unreadable),
            },
            '"' => quoted = !quoted,
            '#' | ';' if !quoted => break,
            c => out.push(c),
        }
        if quoted || !out.ends_with(char::is_whitespace) {
            kept = out.len();
        }
    }
    if quoted {
        return Err(RepoConfigRefusal::Unreadable);
    }
    out.truncate(kept);
    Ok(out)
}

#[cfg(test)]
#[path = "repo_config_tests.rs"]
mod tests;
