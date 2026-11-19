// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{LinkPolicy, link_policy, unescape_destination};
use std::{collections::BTreeMap, sync::Arc};

/// The first definition of each normalized label. A long URI is shared by every
/// use, and its policy is classified once, so repeated references cost O(uses).
#[derive(Default)]
pub(super) struct Definitions {
    targets: BTreeMap<String, Target>,
}

#[derive(Clone)]
pub(super) struct Target {
    pub destination: Arc<str>,
    pub policy: LinkPolicy,
}

impl Target {
    /// URI and email autolinks carry their literal destinations.
    pub fn new(destination: &str) -> Self {
        Self {
            destination: Arc::from(destination),
            policy: link_policy(destination),
        }
    }

    /// Links and images interpret Markdown escapes before classifying the URI.
    pub fn authored(destination: &str) -> Self {
        Self::new(&unescape_destination(destination))
    }
}

impl Definitions {
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    /// Read a definition only where the block parser could start a paragraph.
    /// Return the number of source bytes to omit, including the final line ending.
    pub fn read(&mut self, source: &str) -> Option<usize> {
        let indent = source.bytes().take_while(|&b| b == b' ').count();
        if indent > 3 {
            return None;
        }
        let (label, suffix) = bracket_label(&source[indent..])?;
        let suffix = suffix.strip_prefix(':')?;
        let suffix = horizontal(suffix);
        let after_indent = line_ending(suffix).map_or(suffix, horizontal);
        let (destination, rest) = definition_destination(after_indent)?;
        let after = horizontal(rest);
        let next = if let Some(next) = line_ending(after) {
            title_line(horizontal(next)).unwrap_or(next)
        } else if after.is_empty() {
            after
        } else if after.len() < rest.len() {
            title_line(after)?
        } else {
            return None;
        };
        self.targets
            .entry(normalize(label)?)
            .or_insert_with(|| Target::authored(destination));
        Some(source.len() - next.len())
    }

    /// Resolve the second label when present, otherwise the text's label.
    /// The returned offset is the end of the usage, independent of the definition.
    pub fn resolve(&self, text: &str, close: usize, label: &str) -> Option<(usize, Target)> {
        if self.is_empty() {
            return None;
        }
        let suffix = &text[close + 1..];
        let (id, rest) = if let Some(rest) = suffix.strip_prefix("[]") {
            (label, rest)
        } else if let Some((id, rest)) = bracket_label(suffix) {
            (id, rest)
        } else {
            (label, suffix)
        };
        let target = self.targets.get(&normalize(id)?)?.clone();
        Some((text.len() - rest.len(), target))
    }
}

fn horizontal(s: &str) -> &str {
    s.trim_start_matches([' ', '\t'])
}

fn line_ending(s: &str) -> Option<&str> {
    s.strip_prefix("\r\n").or_else(|| s.strip_prefix('\n'))
}

fn bracket_label(s: &str) -> Option<(&str, &str)> {
    let body = s.strip_prefix('[')?;
    let mut escaped = false;
    for (i, c) in body.char_indices().take(1000) {
        if !escaped {
            match c {
                '[' => return None,
                ']' => return Some((&body[..i], &body[i + 1..])),
                '\n' if horizontal(&body[i + 1..]).starts_with(['\n', '\r']) => return None,
                _ => {}
            }
        }
        escaped = c == '\\' && !escaped;
    }
    None
}

/// Preserve authored escapes when comparing labels. Fold through lowercase then
/// uppercase to equate expanding variants such as sharp S and medial/final sigma.
fn normalize(label: &str) -> Option<String> {
    let mut key = String::new();
    let mut space = false;
    let mut escaped = false;
    for (index, c) in label.chars().enumerate() {
        if index >= 999 || matches!(c, '[' | ']') && !escaped {
            return None;
        }
        escaped = c == '\\' && !escaped;
        if matches!(c, ' ' | '\t' | '\r' | '\n') {
            space = !key.is_empty();
        } else {
            if space {
                key.push(' ');
                space = false;
            }
            if c == 'ı' {
                key.push(c);
            } else {
                key.extend(c.to_lowercase().flat_map(char::to_uppercase));
            }
        }
    }
    (!key.is_empty()).then_some(key)
}

fn definition_destination(s: &str) -> Option<(&str, &str)> {
    let angled = s.starts_with('<');
    let body = if angled { &s[1..] } else { s };
    let mut depth = 0usize;
    let mut escaped = false;
    for (i, c) in body.char_indices() {
        if !escaped && angled && c == '>' {
            return Some((&body[..i], &body[i + 1..]));
        }
        if angled && (matches!(c, '\n' | '\r') || c == '<' && !escaped) {
            return None;
        }
        if !angled && (c.is_ascii_whitespace() || c.is_ascii_control()) {
            return (i > 0 && depth == 0).then_some((&body[..i], &body[i..]));
        }
        if !angled && !escaped {
            match c {
                '(' => depth += 1,
                ')' => depth = depth.checked_sub(1)?,
                _ => {}
            }
        }
        escaped = c == '\\' && !escaped;
    }
    (!angled && !body.is_empty() && depth == 0).then_some((body, ""))
}

fn title_line(s: &str) -> Option<&str> {
    let close = match s.as_bytes().first()? {
        b'"' => '"',
        b'\'' => '\'',
        b'(' => ')',
        _ => return None,
    };
    let mut escaped = false;
    for (i, c) in s[1..].char_indices() {
        if c == '\n' && horizontal(&s[i + 2..]).starts_with(['\n', '\r']) {
            return None;
        }
        if !escaped && c == close {
            let rest = horizontal(&s[i + 2..]);
            return line_ending(rest).or_else(|| rest.is_empty().then_some(rest));
        }
        if !escaped && close == ')' && c == '(' {
            return None;
        }
        escaped = c == '\\' && !escaped;
    }
    None
}

#[cfg(test)]
#[path = "references_tests.rs"]
mod tests;
