// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The publication engine's `bin/pub::md_dead_links`, read at
//! 5d4d521b94746d3d51e7f06fe8375a4d167d51ae. The target recognizers below are
//! reused from its `crates/pub/src/guards/private_refs.rs`, element for element.
//! Missing in dev is informational; boundary-excluded and escaping targets fail.
//! A virtual export namespace avoids copying unrelated file contents. Its directories
//! and symlinks give `resolve(strict=False)` and `exists()` their exported meaning.

use std::collections::{BTreeMap, BTreeSet};

use super::{Engine, Finding, Guard, Listing, exported, org, selects};
use crate::export::{MANIFEST, PRIVATE_ORG, Selection};

/// Work and verdict of the offline dead-link guard.
#[derive(Default)]
pub struct Scanned {
    pub documents: usize,
    pub bytes: usize,
    pub targets: usize,
    pub upstream: usize,
    pub findings: Vec<Finding>,
}

#[derive(Default)]
struct Namespace {
    nodes: BTreeSet<String>,
    dirs: BTreeSet<String>,
    links: BTreeMap<String, String>,
}

impl Namespace {
    fn insert(&mut self, rel: &str) {
        self.nodes.insert(rel.to_string());
        let mut path = rel;
        while let Some((parent, _)) = path.rsplit_once('/') {
            self.nodes.insert(parent.to_string());
            self.dirs.insert(parent.to_string());
            path = parent;
        }
        self.nodes.insert(String::new());
        self.dirs.insert(String::new());
    }

    // Resolve components before `..`, just as pathlib does; an absent component
    // remains in the path. A loop leaves the unresolved remainder in place.
    fn resolve(
        &self,
        path: &str,
        seen: &mut BTreeSet<String>,
        deps: &mut Vec<String>,
    ) -> Option<(String, bool)> {
        let mut parts: Vec<String> = Vec::new();
        let mut exists = true;
        for component in path.split('/') {
            if !component.is_empty() {
                exists &= self.dirs.contains(&parts.join("/"));
            }
            match component {
                "" | "." => continue,
                ".." => {
                    parts.pop()?;
                }
                name => parts.push(name.to_string()),
            }
            let rel = parts.join("/");
            if let Some(target) = self.links.get(&rel) {
                deps.push(rel.clone());
                if !seen.insert(rel.clone()) {
                    exists = false;
                    continue;
                }
                if target.starts_with('/') {
                    return None;
                }
                parts.pop();
                let joined = format!("{}/{target}", parts.join("/"));
                let (resolved, target_exists) = self.resolve(&joined, seen, deps)?;
                exists &= target_exists;
                seen.remove(&rel);
                parts = resolved
                    .split('/')
                    .filter(|p| !p.is_empty())
                    .map(str::to_string)
                    .collect();
            } else {
                exists &= self.nodes.contains(&rel);
            }
        }
        let resolved = parts.join("/");
        deps.push(resolved.clone());
        Some((resolved, exists))
    }
}

fn affected(changed: Option<&[String]>, md: &str, deps: &[String]) -> bool {
    changed.is_none_or(|paths| {
        paths.iter().any(|p| {
            p == md
                || deps.iter().any(|d| {
                    d == p || d.starts_with(&format!("{p}/")) || p.starts_with(&format!("{d}/"))
                })
        })
    })
}

/// Judge shipping Markdown and its inbound change cone. `listing.paths` must name
/// the whole checkout, including excluded files; `changed = None` judges all links.
/// Only Markdown bytes and namespace metadata are read, never unrelated contents.
/// No network, subprocess, or exported copy is needed.
#[must_use]
pub fn scan(listing: &Listing<'_>, engine: Option<&Engine>, changed: Option<&[String]>) -> Scanned {
    let mut out = Scanned::default();
    let selection = match crate::tree::read_to_string(&listing.root.join(MANIFEST)) {
        Ok(text) => match Selection::parse(&text) {
            Ok(selection) => selection,
            Err(_) => return out, // The content scan reports the invalid manifest.
        },
        Err(_) => Selection::everything(),
    };
    let mut dev = Namespace::default();
    let mut exp = Namespace::default();
    let mut documents = Vec::new();
    for rel in listing.paths {
        let Ok(meta) = std::fs::symlink_metadata(listing.root.join(rel)) else {
            continue;
        };
        dev.insert(rel);
        if !selects(&selection, rel) {
            continue;
        }
        exp.insert(rel);
        if meta.file_type().is_symlink() {
            if let Ok(target) = std::fs::read_link(listing.root.join(rel)) {
                exp.links
                    .insert(rel.clone(), target.to_string_lossy().into_owned());
            }
        } else if meta.is_file() && rel.ends_with(".md") {
            documents.push(rel);
        }
    }
    let org = engine.map(|e| org::Rewrite::new(PRIVATE_ORG, &e.names));
    for md in documents {
        // The engine's Markdown reader has no size ceiling. The content census's
        // 64 MiB reader would silently miss a boundary link in a larger document.
        let Ok(bytes) = std::fs::read(listing.root.join(md)) else {
            continue;
        };
        let bytes = exported(md, &bytes, org.as_ref());
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        // Python's text reader normalizes CRLF and bare CR before the regex.
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        out.documents += 1;
        out.bytes += bytes.len();
        let mut targets = md_link_targets(&text);
        targets.extend(md_ref_targets(&text));
        for target in targets {
            if target.contains("://")
                || ["mailto:", "//", "tel:"]
                    .iter()
                    .any(|p| target.starts_with(p))
            {
                continue;
            }
            let path = if target.starts_with('/') {
                target.trim_start_matches('/').to_string()
            } else {
                format!(
                    "{}{target}",
                    md.rsplit_once('/')
                        .map_or(String::new(), |(p, _)| format!("{p}/"))
                )
            };
            let mut deps = vec![path.clone()];
            let resolved = exp.resolve(&path, &mut BTreeSet::new(), &mut deps);
            if !affected(changed, md, &deps) {
                continue;
            }
            out.targets += 1;
            let detail = if target.contains('\0') {
                Some(format!(
                    "invalid link target (embedded NUL): {}",
                    repr(target)
                ))
            } else if let Some((rel, exists)) = resolved {
                if exists {
                    continue;
                }
                if listing
                    .gitlinks
                    .iter()
                    .any(|g| rel == *g || rel.starts_with(&format!("{g}/")))
                {
                    continue;
                }
                if dev.nodes.contains(&rel) {
                    Some(format!(
                        "dead link (excluded by the publish boundary): {target}"
                    ))
                } else {
                    out.upstream += 1;
                    None
                }
            } else {
                Some(format!("link escapes the export: {target}"))
            };
            if let Some(detail) = detail {
                out.findings.push(Finding {
                    guard: Guard::DeadLinks,
                    path: md.clone(),
                    line: 0,
                    detail,
                });
            }
        }
    }
    out
}

// Python's repr quoting for a link target (the regex already excludes whitespace).
fn repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            '\0'..='\u{1f}' | '\u{7f}' => out.push_str(&format!("\\x{:02x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}
/// Python's `\s` for a `str` pattern: `Py_UNICODE_ISSPACE`, not just ASCII.
/// Verified: `[a](\u{a0}docs/x.md)` captures `docs/x.md`.
fn is_py_space(c: char) -> bool {
    matches!(c,
        '\u{09}'..='\u{0d}' | '\u{1c}'..='\u{1f}' | ' ' | '\u{85}' | '\u{a0}'
        | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}'
        | '\u{202f}' | '\u{205f}' | '\u{3000}')
}
fn char_at(text: &str, i: usize) -> Option<char> {
    text.get(i..).and_then(|s| s.chars().next())
}
fn skip_space(text: &str, mut p: usize) -> usize {
    while let Some(c) = char_at(text, p) {
        if is_py_space(c) {
            p += c.len_utf8();
        } else {
            break;
        }
    }
    p
}
/// `\]\(\s*<?([^)\s>#?]+)>?(?:\s+"[^"]*")?\s*(?:[#?][^)]*)?\)`
fn md_link_targets(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < text.len() {
        if bytes[i] == b']'
            && bytes.get(i + 1) == Some(&b'(')
            && let Some((capture, end)) = md_link_at(text, i)
        {
            out.push(capture);
            i = end;
            continue;
        }
        i += char_at(text, i).map_or(1, char::len_utf8);
    }
    out
}

fn md_link_at(text: &str, at: usize) -> Option<(&str, usize)> {
    // `\s*` needs no backtracking: everything that can follow it (`<`, or the
    // capture class) excludes whitespace, so only the maximal run can lead to a
    // match.
    let ws = skip_space(text, at + 2);

    // `<?` is greedy — it takes the bracket first. When the capture then fails
    // (as in `[a](<>)`, where `>` is excluded), it gives the bracket back and the
    // capture class swallows the `<` itself.
    let mut group_starts = Vec::with_capacity(2);
    if char_at(text, ws) == Some('<') {
        group_starts.push(ws + 1);
    }
    group_starts.push(ws);

    for start in group_starts {
        let mut end = start;
        while let Some(c) = char_at(text, end) {
            if c == ')' || c == '>' || c == '#' || c == '?' || is_py_space(c) {
                break;
            }
            end += c.len_utf8();
        }
        // `+` is greedy: longest first, down to one character.
        let mut cut = end;
        while cut > start {
            if let Some(finish) = md_link_tail(text, cut) {
                return Some((&text[start..cut], finish));
            }
            cut -= text[start..cut]
                .chars()
                .next_back()
                .map_or(1, char::len_utf8);
        }
    }
    None
}

/// `>?(?:\s+"[^"]*")?\s*(?:[#?][^)]*)?\)` — everything after the capture.
fn md_link_tail(text: &str, from: usize) -> Option<usize> {
    let mut gt_options = Vec::with_capacity(2);
    if char_at(text, from) == Some('>') {
        gt_options.push(from + 1);
    }
    gt_options.push(from);

    for after_gt in gt_options {
        let mut title_options = Vec::with_capacity(2);
        if let Some(after_title) = md_link_title(text, after_gt) {
            title_options.push(after_title);
        }
        title_options.push(after_gt);

        for after_title in title_options {
            let after_ws = skip_space(text, after_title);
            let mut frag_options = Vec::with_capacity(2);
            if matches!(char_at(text, after_ws), Some('#' | '?')) {
                let mut u = after_ws + 1;
                while let Some(c) = char_at(text, u) {
                    if c == ')' {
                        break;
                    }
                    u += c.len_utf8();
                }
                frag_options.push(u);
            }
            frag_options.push(after_ws);

            for after_frag in frag_options {
                if char_at(text, after_frag) == Some(')') {
                    return Some(after_frag + 1);
                }
            }
        }
    }
    None
}

/// `(?:\s+"[^"]*")?` — double quotes only. `[a](x.md 'Title')` does not match at
/// all, so a single-quoted title makes the whole link invisible to the guard.
fn md_link_title(text: &str, from: usize) -> Option<usize> {
    let after_ws = skip_space(text, from);
    if after_ws == from {
        return None; // `\s+` needs at least one
    }
    if char_at(text, after_ws) != Some('"') {
        return None;
    }
    let mut u = after_ws + 1;
    while let Some(c) = char_at(text, u) {
        if c == '"' {
            return Some(u + 1);
        }
        u += c.len_utf8();
    }
    None
}

/// `^\[[^\]]+\]:\s*<?([^\s>#?]+)` with `re.M`.
fn md_ref_targets(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut resume = 0usize;
    // `^` matches at offset 0 and immediately after every `\n`.
    let mut starts = vec![0usize];
    starts.extend(
        text.bytes()
            .enumerate()
            .filter(|(_, b)| *b == b'\n')
            .map(|(i, _)| i + 1),
    );
    for start in starts {
        if start < resume || start >= text.len() {
            continue;
        }
        if let Some((capture, end)) = md_ref_at(text, start) {
            out.push(capture);
            resume = end;
        }
    }
    out
}

fn md_ref_at(text: &str, at: usize) -> Option<(&str, usize)> {
    if char_at(text, at) != Some('[') {
        return None;
    }
    // `[^\]]+` matches newlines too, so a definition label may span lines. It
    // cannot cross a `]`, so the greedy run is the only candidate.
    let label_start = at + 1;
    let mut p = label_start;
    while let Some(c) = char_at(text, p) {
        if c == ']' {
            break;
        }
        p += c.len_utf8();
    }
    if p == label_start || char_at(text, p) != Some(']') {
        return None;
    }
    p += 1;
    if char_at(text, p) != Some(':') {
        return None;
    }
    p += 1;
    // `\s*` crosses newlines: `[r]:\n\ndocs/x.md` still captures the target.
    let ws = skip_space(text, p);

    let mut starts = Vec::with_capacity(2);
    if char_at(text, ws) == Some('<') {
        starts.push(ws + 1);
    }
    starts.push(ws);
    for start in starts {
        let mut end = start;
        while let Some(c) = char_at(text, end) {
            // Note `)` is NOT excluded here, unlike MD_LINK.
            if c == '>' || c == '#' || c == '?' || is_py_space(c) {
                break;
            }
            end += c.len_utf8();
        }
        if end > start {
            return Some((&text[start..end], end));
        }
    }
    None
}
