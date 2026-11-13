// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! credential-claim: an open issue that says an owner's or a person's credential is needed names
//! the secrets-registry entry it checked, or the `aterm secrets need` request filed for one the
//! registry holds none of. A credential is never a person's to give (the harness takes such a
//! `NEEDS YOU` line off the person's list): on 2026-11-12 an entry recorded "needs the owner's
//! release credential" while the vault held `release-notary`, the Developer ID certificate and key
//! and the release-signing profile, and agents carried that claim to the person.
//!
//! A claim is a sentence, read outside inline code, fenced code and HTML comments, that holds a
//! possessive owner or person (`owner's`, `person's`) and a credential word (credential, token,
//! key, login, password, certificate). Its evidence is a sentence of the same entry that says
//! `registry` and names an entry in inline code, or names `aterm secrets need` in inline code.

use super::conflict_marker::{fence_closes, fence_open};
use super::{Finding, Rule, Tree};

pub(super) const CREDENTIAL_CLAIM: Rule = Rule {
    id: "credential-claim",
    why: "an open issue that claims a person's credential is needed, without the registry entry or \
          the `aterm secrets need` request, sends agents to ask a person for a credential",
    scope: &[],
    check: credential_claim,
};

const INDEX: &str = "docs/KNOWN_ISSUES.md";

/// The possessives that make a sentence say the credential is a person's.
const WHOSE: &[&str] = &["owner's", "owner\u{2019}s", "person's", "person\u{2019}s"];

/// The nouns that make a possessive's phrase a credential.
const CREDENTIAL_WORDS: &[&str] = &[
    "credential",
    "credentials",
    "token",
    "tokens",
    "login",
    "logins",
    "password",
    "passwords",
    "certificate",
    "certificates",
];

/// The words that make a `key` a credential's, not a keyboard's.
const KEY_KINDS: &[&str] = &[
    "signing", "release", "deploy", "api", "ssh", "private", "notary", "gpg", "access", "secret",
];

/// The most words after the possessive its noun phrase takes.
const PHRASE_WORDS: usize = 4;

/// The request, in inline code, filed for a credential the registry holds none of.
const NEED_REQUEST: &str = "aterm secrets need ";

/// One entry's prose: its characters, each with the line it came from, inline code masked.
struct Prose {
    /// Each character of the entry's prose lines, joined by a space, with the 1-based line it is on.
    chars: Vec<(char, usize)>,
}

impl Prose {
    /// The entry's sentences: each a range of [`Self::chars`], split after `.`, `!` or `?` and a
    /// space, never inside inline code.
    fn sentences(&self) -> Vec<std::ops::Range<usize>> {
        let mut out = Vec::new();
        let mut start = 0;
        let mut code = false;
        for (i, (c, _)) in self.chars.iter().enumerate() {
            if *c == '`' {
                code = !code;
                continue;
            }
            let ends = !code
                && matches!(c, '.' | '!' | '?')
                && self.chars.get(i + 1).is_none_or(|(n, _)| n.is_whitespace());
            if ends {
                out.push(start..i + 1);
                start = i + 1;
            }
        }
        if start < self.chars.len() {
            out.push(start..self.chars.len());
        }
        out
    }

    /// Sentence `range` lower-cased with inline code cut, and its inline code spans.
    fn read(&self, range: std::ops::Range<usize>) -> (String, Vec<String>) {
        let mut text = String::new();
        let mut quoted: Vec<String> = Vec::new();
        let mut code = false;
        for (c, _) in &self.chars[range] {
            if *c == '`' {
                code = !code;
                if code {
                    quoted.push(String::new());
                }
                text.push(' ');
                continue;
            }
            if let (true, Some(span)) = (code, quoted.last_mut()) {
                span.push(*c);
            } else {
                text.extend(c.to_lowercase());
            }
        }
        (text, quoted)
    }
}

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
}

/// Whether `text` (lower-cased, inline code cut) claims a person's credential: a possessive owner
/// or person whose phrase, the next [`PHRASE_WORDS`] words, names a credential, or a key of a
/// credential's kind (a person's keys alone are a keyboard's).
fn claims(text: &str) -> bool {
    let all: Vec<&str> = words(text).collect();
    all.iter().enumerate().any(|(i, w)| {
        let whose = matches!(*w, "owner" | "person")
            && WHOSE.iter().any(|p| text.contains(p))
            && all.get(i + 1) == Some(&"s");
        if !whose {
            return false;
        }
        let phrase = &all[(i + 2).min(all.len())..(i + 2 + PHRASE_WORDS).min(all.len())];
        phrase.iter().enumerate().any(|(j, n)| {
            CREDENTIAL_WORDS.contains(n)
                || (matches!(*n, "key" | "keys") && j > 0 && KEY_KINDS.contains(&phrase[j - 1]))
        })
    })
}

/// Whether a sentence (`text`, its inline code spans `quoted`) names the registry entry it checked
/// (a span that is no `aterm` command), or the `aterm secrets need` request filed for one the
/// registry holds none of.
fn checked(text: &str, quoted: &[String]) -> bool {
    let registry = words(text).any(|w| w == "registry");
    quoted.iter().any(|span| {
        span.starts_with(NEED_REQUEST)
            || (registry && !span.trim().is_empty() && !span.starts_with("aterm "))
    })
}

fn block_line(line: &str) -> Option<&str> {
    let indent = line.bytes().take_while(|&byte| byte == b' ').count();
    (indent <= 3).then_some(&line[indent..])
}

fn heading(line: &str) -> Option<&str> {
    let after = block_line(line)?.strip_prefix("## ")?;
    after
        .split_whitespace()
        .next()
        .filter(|id| id.starts_with("ATERM-KI-"))
}

fn credential_claim(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let Some(text) = tree.text(INDEX) else {
        return;
    };
    let mut entries: Vec<(String, Prose)> = Vec::new();
    let mut fence = None;
    let mut comment = false;
    for (at, line) in text.lines().enumerate() {
        if let Some((ch, length)) = fence {
            if fence_closes(line, ch, length) {
                fence = None;
            }
            continue;
        }
        if comment {
            comment = !line.contains("-->");
            continue;
        }
        if block_line(line).is_some_and(|l| l.starts_with("<!--")) {
            comment = !line.contains("-->");
            continue;
        }
        if let Some(open) = fence_open(line) {
            fence = Some(open);
            continue;
        }
        if let Some(id) = heading(line) {
            entries.push((id.to_owned(), Prose { chars: Vec::new() }));
            continue;
        }
        if line.starts_with('#') || line.starts_with("    ") || line.starts_with('\t') {
            continue;
        }
        let Some((_, prose)) = entries.last_mut() else {
            continue;
        };
        if line.trim().is_empty() {
            // A paragraph ends its sentence.
            if prose.chars.last().is_some_and(|(c, _)| *c != '.') {
                prose.chars.push(('.', at + 1));
            }
            prose.chars.push((' ', at + 1));
            continue;
        }
        if !prose.chars.is_empty() {
            prose.chars.push((' ', at + 1));
        }
        prose.chars.extend(line.trim().chars().map(|c| (c, at + 1)));
    }
    for (id, prose) in &entries {
        let sentences = prose.sentences();
        let read: Vec<(String, Vec<String>)> =
            sentences.iter().map(|s| prose.read(s.clone())).collect();
        if read.iter().any(|(text, quoted)| checked(text, quoted)) {
            continue;
        }
        for (range, (text, _)) in sentences.iter().zip(&read) {
            if !claims(text) {
                continue;
            }
            // The line the possessive stands on.
            let line = (range.start..range.end)
                .find(|&i| {
                    let ahead: String = prose.chars[i..range.end]
                        .iter()
                        .take(8)
                        .flat_map(|(c, _)| c.to_lowercase())
                        .collect();
                    WHOSE.iter().any(|w| ahead.starts_with(w))
                })
                .map_or(prose.chars[range.start].1, |i| prose.chars[i].1);
            out.push(Finding {
                rule: CREDENTIAL_CLAIM.id,
                path: INDEX.to_owned(),
                line,
                detail: format!(
                    "{id} says an owner's or person's credential is needed: a credential is no \
                     person's to give. Name the entry `aterm secrets ls --describe` shows in a \
                     sentence that says registry, or the `aterm secrets need <want>` request filed \
                     for one it does not hold"
                ),
            });
        }
    }
}

#[cfg(test)]
#[path = "credential_claim_tests.rs"]
mod tests;
