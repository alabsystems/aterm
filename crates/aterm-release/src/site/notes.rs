// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `/releases`: the newest release notes, rendered from the clone's CHANGELOG.md
//! when it has one, else from the GitHub release bodies (the public snapshot exports
//! no changelog). A deliberately small Markdown subset — headings, paragraphs,
//! bullet and numbered lists (continuation lines and nesting), fenced code, block
//! quotes, rules; inline `code`, **bold**, [links](https://…) — and everything else
//! escaped text. Same input, same bytes.

use super::html_escape as esc;
use super::releases::{Release, stable, version_key};

/// How many sections the page carries.
pub const LIMIT: usize = 12;

/// One release's notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub tag: String,
    pub date: String,
    pub body: String,
}

/// `` `[^`]+` `` pieces of `text`, alternating with the text between them, as a
/// capturing split yields them.
fn code_split(text: &str) -> Vec<&str> {
    let b = text.as_bytes();
    let mut pieces = Vec::new();
    let (mut last, mut i) = (0, 0);
    while i < b.len() {
        if b[i] == b'`'
            && i + 1 < b.len()
            && b[i + 1] != b'`'
            && let Some(len) = text[i + 1..].find('`')
        {
            let end = i + 1 + len + 1;
            pieces.push(&text[last..i]);
            pieces.push(&text[i..end]);
            last = end;
            i = end;
            continue;
        }
        i += 1;
    }
    pieces.push(&text[last..]);
    pieces
}

/// `[TEXT](http(s)://URL)` → `<a href="URL">TEXT</a>`, over already-escaped text.
fn links(t: &str) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < t.len() {
        let rest = &t[i..];
        let hit = rest.starts_with('[').then(|| {
            let close = rest[1..].find(']').map(|n| n + 1).filter(|n| *n > 1)?;
            let after = rest[close + 1..].strip_prefix('(')?;
            let scheme = ["http://", "https://"]
                .iter()
                .find(|s| after.starts_with(*s))?;
            let url_len = after
                .char_indices()
                .find(|(_, c)| c.is_whitespace() || *c == ')')
                .map_or(after.len(), |(n, _)| n);
            (url_len > scheme.len() && after[url_len..].starts_with(')')).then(|| {
                let url = &after[..url_len];
                (
                    format!("<a href=\"{url}\">{}</a>", &rest[1..close]),
                    close + 2 + url_len + 1,
                )
            })
        });
        match hit.flatten() {
            Some((html, len)) => {
                out.push_str(&html);
                i += len;
            }
            None => {
                let c = rest.chars().next().unwrap_or_default();
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    out
}

/// `**TEXT**` (non-greedy, one line) → `<strong>TEXT</strong>`.
fn bold(t: &str) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < t.len() {
        let rest = &t[i..];
        let hit = rest.starts_with("**").then(|| {
            let body = &rest[2..];
            let first = body.chars().next().filter(|c| *c != '\n')?;
            let close = body[first.len_utf8()..].find("**")? + first.len_utf8();
            (!body[..close].contains('\n')).then_some(close)
        });
        match hit.flatten() {
            Some(close) => {
                out.push_str("<strong>");
                out.push_str(&rest[2..2 + close]);
                out.push_str("</strong>");
                i += 2 + close + 2;
            }
            None => {
                let c = rest.chars().next().unwrap_or_default();
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    out
}

/// Escape everything, then recognise `code`, [links](https://…) and **bold**. Code
/// spans are lifted out first (a NUL-framed index; NUL is stripped from the input) so
/// a bold run may CONTAIN a code span, and a link or bold marker inside a code span
/// stays literal.
fn inline(text: &str) -> String {
    let text = text.replace('\0', "");
    let mut codes = Vec::new();
    let mut parts = String::new();
    for piece in code_split(&text) {
        if piece.len() >= 2 && piece.starts_with('`') && piece.ends_with('`') {
            codes.push(format!("<code>{}</code>", esc(&piece[1..piece.len() - 1])));
            parts.push_str(&format!("\0{}\0", codes.len() - 1));
        } else {
            parts.push_str(&esc(piece));
        }
    }
    let t = bold(&links(&parts));
    let mut out = String::new();
    let mut rest = t.as_str();
    while let Some(at) = rest.find('\0') {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 1..];
        let n = tail.bytes().take_while(u8::is_ascii_digit).count();
        match (tail[..n].parse::<usize>().ok(), tail[n..].starts_with('\0')) {
            (Some(k), true) if k < codes.len() => {
                out.push_str(&codes[k]);
                rest = &tail[n + 1..];
            }
            _ => {
                out.push('\0');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `^(#{1,6})\s+(.*?)\s*#*\s*$`: the level and the text.
fn heading(line: &str) -> Option<(usize, &str)> {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &line[hashes..];
    let text = rest.trim_start();
    if text.len() == rest.len() {
        return None;
    }
    Some((hashes, text.trim_end().trim_end_matches('#').trim_end()))
}

/// `^\s*(-{3,}|\*{3,}|_{3,})\s*$`.
fn rule(line: &str) -> bool {
    let t = line.trim();
    t.len() >= 3 && ['-', '*', '_'].iter().any(|c| t.chars().all(|x| x == *c))
}

/// `^(\s*)([-*+]|\d+[.)])\s+(.*)$`: the indent (tabs to 4), ordered, the text.
fn item(line: &str) -> Option<(usize, bool, &str)> {
    let rest = line.trim_start();
    let lead = &line[..line.len() - rest.len()];
    let marker = if rest.starts_with(['-', '*', '+']) {
        1
    } else {
        let d = rest.bytes().take_while(u8::is_ascii_digit).count();
        (d > 0 && rest[d..].starts_with(['.', ')'])).then_some(d + 1)?
    };
    let after = &rest[marker..];
    let text = after.trim_start();
    if text.len() == after.len() {
        return None;
    }
    let mut indent = 0;
    for c in lead.chars() {
        indent = if c == '\t' {
            (indent / 4 + 1) * 4
        } else {
            indent + 1
        };
    }
    Some((indent, rest[marker - 1..].starts_with(['.', ')']), text))
}

fn fence(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

/// `^>\s?(.*)$`.
fn quote(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('>')?;
    Some(match rest.chars().next() {
        Some(c) if c.is_whitespace() => &rest[c.len_utf8()..],
        _ => rest,
    })
}

/// `items` (indent, ordered, text) → nested `<ul>`/`<ol>`: one level per distinct
/// indent, deeper indents nested under the previous item.
fn render_list(items: &[(usize, bool, String)]) -> String {
    let mut out = String::new();
    let mut stack: Vec<(usize, &str)> = Vec::new();
    for (indent, ordered, text) in items {
        let tag = if *ordered { "ol" } else { "ul" };
        match stack.last() {
            Some((top, _)) if *indent <= *top => {
                while stack.last().is_some_and(|(i, _)| i > indent) {
                    let (_, t) = stack.pop().unwrap_or_default();
                    out.push_str(&format!("</li></{t}>"));
                }
                if stack.is_empty() {
                    out.push_str(&format!("<{tag}><li>{}", inline(text)));
                    stack.push((*indent, tag));
                } else {
                    out.push_str(&format!("</li><li>{}", inline(text)));
                }
            }
            _ => {
                out.push_str(&format!("<{tag}><li>{}", inline(text)));
                stack.push((*indent, tag));
            }
        }
    }
    while let Some((_, t)) = stack.pop() {
        out.push_str(&format!("</li></{t}>"));
    }
    out
}

/// The Markdown subset (module docs) → HTML blocks, one per line.
#[must_use]
pub fn render_markdown(md: &str) -> String {
    let md = md.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = md.split('\n').collect();
    let mut blocks = Vec::new();
    let mut i = 0;
    let n = lines.len();
    while i < n {
        let line = lines[i];
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        if fence(line) {
            i += 1;
            let mut code = Vec::new();
            while i < n && !fence(lines[i]) {
                code.push(lines[i]);
                i += 1;
            }
            i += 1;
            blocks.push(format!("<pre><code>{}</code></pre>", esc(&code.join("\n"))));
            continue;
        }
        if let Some((hashes, text)) = heading(line) {
            let level = if hashes <= 3 { 3 } else { 4 };
            blocks.push(format!("<h{level}>{}</h{level}>", inline(text)));
            i += 1;
            continue;
        }
        if rule(line) {
            blocks.push("<hr />".into());
            i += 1;
            continue;
        }
        if item(line).is_some() {
            let mut items: Vec<(usize, bool, String)> = Vec::new();
            while i < n && !lines[i].trim().is_empty() {
                if let Some((indent, ordered, text)) = item(lines[i]) {
                    items.push((indent, ordered, text.to_string()));
                } else if let Some(last) = items.last_mut() {
                    last.2.push(' ');
                    last.2.push_str(lines[i].trim());
                } else {
                    break;
                }
                i += 1;
            }
            blocks.push(render_list(&items));
            continue;
        }
        if quote(line).is_some() {
            let mut quoted = Vec::new();
            while let Some(q) = lines.get(i).and_then(|l| quote(l)) {
                quoted.push(q);
                i += 1;
            }
            blocks.push(format!(
                "<blockquote>{}</blockquote>",
                render_markdown(&quoted.join("\n"))
            ));
            continue;
        }
        let mut para = Vec::new();
        while i < n
            && !lines[i].trim().is_empty()
            && heading(lines[i]).is_none()
            && !fence(lines[i])
            && !rule(lines[i])
            && item(lines[i]).is_none()
            && quote(lines[i]).is_none()
        {
            para.push(lines[i].trim());
            i += 1;
        }
        blocks.push(format!("<p>{}</p>", inline(&para.join(" "))));
    }
    blocks.join("\n")
}

/// `## [X.Y.Z] - YYYY-MM-DD`: the version and the date.
fn changelog_head(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("## [")?;
    let (version, rest) = rest.split_once(']')?;
    let parts: Vec<&str> = version.split('.').collect();
    let numeric = parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    let date = rest.trim_start().strip_prefix('-')?.trim();
    let date_ok = date.len() == 10
        && date.bytes().enumerate().all(|(k, b)| {
            if k == 4 || k == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        });
    (numeric && date_ok).then_some((version, date))
}

/// The `## [X.Y.Z] - YYYY-MM-DD` sections of a changelog, newest version first.
/// `[Unreleased]` and anything before the first versioned heading are not release
/// notes.
#[must_use]
pub fn from_changelog(text: &str) -> Vec<Section> {
    let text = text.replace("\r\n", "\n");
    let mut sections: Vec<(Section, Vec<&str>)> = Vec::new();
    let mut open = false;
    for line in text.split('\n') {
        if line.starts_with("## ") {
            open = match changelog_head(line) {
                Some((version, date)) => {
                    sections.push((
                        Section {
                            tag: format!("v{version}"),
                            date: date.into(),
                            body: String::new(),
                        },
                        Vec::new(),
                    ));
                    true
                }
                None => false,
            };
            continue;
        }
        if open && let Some((_, lines)) = sections.last_mut() {
            lines.push(line);
        }
    }
    let mut out: Vec<Section> = sections
        .into_iter()
        .map(|(mut s, lines)| {
            s.body = lines.join("\n").trim_matches('\n').to_string();
            s
        })
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(version_key(&s.tag)));
    out
}

/// A body without the cutter's "what each file is" legend and the rule after it —
/// when, and only when, that exact shape leads the body.
fn strip_asset_preamble(body: &str) -> String {
    let lead = body.trim_start();
    let legend = lead.starts_with("**aterm** is ")
        && lead["**aterm** is ".len()..].contains("New here? What each file is");
    if !legend {
        return body.to_string();
    }
    let normalized = body.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    match lines.iter().take(40).position(|l| rule(l)) {
        Some(idx) => lines[idx + 1..].join("\n").trim_matches('\n').to_string(),
        None => body.to_string(),
    }
}

/// The stable releases' bodies, newest version first.
#[must_use]
pub fn from_releases(releases: &[Release]) -> Vec<Section> {
    stable(releases)
        .into_iter()
        .map(|r| Section {
            tag: r.tag.clone(),
            date: r.date.clone(),
            body: strip_asset_preamble(&r.body),
        })
        .collect()
}

/// The page's footer, as the rest of the site carries it.
const SOCIAL_FOOTER: &str = concat!(
    "    <div class=\"by\">\n",
    "      <p><b class=\"mark\">ALab</b> &middot; <a href=\"/\">alab.systems</a> &middot; By Andrew Yates &middot; <a href=\"mailto:hello@alab.systems\">hello@alab.systems</a> &middot; &copy;&nbsp;2026\n",
    "        &middot; <a class=\"soc\" href=\"https://www.linkedin.com/in/alabsystems/\" aria-label=\"Andrew Yates on LinkedIn\"><svg viewBox=\"0 0 24 24\" width=\"16\" height=\"16\" aria-hidden=\"true\" focusable=\"false\"><path fill=\"currentColor\" d=\"M20.447 20.452h-3.554v-5.569c0-1.328-.027-3.037-1.852-3.037-1.853 0-2.136 1.445-2.136 2.939v5.667H9.351V9h3.414v1.561h.046c.477-.9 1.637-1.85 3.37-1.85 3.601 0 4.267 2.37 4.267 5.455v6.286zM5.337 7.433c-1.144 0-2.063-.926-2.063-2.065 0-1.138.92-2.063 2.063-2.063 1.14 0 2.064.925 2.064 2.063 0 1.139-.925 2.065-2.064 2.065zm1.782 13.019H3.555V9h3.564v11.452zM22.225 0H1.771C.792 0 0 .774 0 1.729v20.542C0 23.227.792 24 1.771 24h20.451C23.2 24 24 23.227 24 22.271V1.729C24 .774 23.2 0 22.222 0h.003z\"/></svg></a>\n",
    "        <a class=\"soc\" href=\"https://x.com/alabsystems\" aria-label=\"Andrew Yates on X\"><svg viewBox=\"0 0 24 24\" width=\"16\" height=\"16\" aria-hidden=\"true\" focusable=\"false\"><path fill=\"currentColor\" d=\"M18.901 1.153h3.68l-8.04 9.19L24 22.846h-7.406l-5.8-7.584-6.638 7.584H.474l8.6-9.83L0 1.154h7.594l5.243 6.932ZM17.61 20.644h2.039L6.486 3.24H4.298Z\"/></svg></a></p>\n",
    "    </div>\n",
);

/// `/assets/releases.css`: the page's own rules; the palette and type come from
/// styles.css.
pub const CSS: &str = "\
/* /releases — GENERATED by aterm's `ship site` (crates/aterm-release/src/site).
   Do not hand-edit: the next sync rewrites it. Page-specific rules only; the
   palette and type families come from styles.css. */
.releases-index { display: flex; flex-wrap: wrap; gap: 0.25rem 0.75rem; font: 400 0.8125rem/1.6 var(--mono); color: var(--muted); }
.releases-index a { color: var(--muted); }
.releases-index a:hover, .releases-index a:focus-visible { color: var(--accent); }
.release { border-top: 1px solid var(--rule); padding-top: 1.25rem; }
.release h2 a { text-decoration: none; }
.release h2 a:hover, .release h2 a:focus-visible { color: var(--accent); }
.release .release-date { font-weight: 400; color: var(--muted); margin-left: 0.5em; }
.release-notes { display: flex; flex-direction: column; gap: 0.6rem; font: 400 0.9375rem/1.6 var(--serif); color: var(--fg-dim); }
.release-notes h3, .release-notes h4 { font: 600 0.875rem var(--mono); letter-spacing: -0.01em; margin: 0.4rem 0 0; color: var(--fg); }
.release-notes ul, .release-notes ol { margin: 0; padding-left: 1.25em; }
.release-notes li { margin: 0; }
.release-notes li + li { margin-top: 0.35rem; }
.release-notes li ul, .release-notes li ol { margin-top: 0.35rem; }
.release-notes code { font: 400 0.9em var(--mono); }
.release-notes pre { margin: 0; padding: 0.6rem 0.8rem; overflow-x: auto; font: 400 0.8125rem/1.5 var(--mono); border: 1px solid var(--rule); }
.release-notes pre code { font: inherit; }
.release-notes blockquote { margin: 0; padding-left: 0.8rem; border-left: 1px solid var(--rule); }
.release-notes hr { border: 0; border-top: 1px solid var(--rule); margin: 0.2rem 0; }
.release-notes strong { color: var(--fg); font-weight: 600; }
";

/// `releases.html` for `sections` (at most [`LIMIT`] of them are given).
#[must_use]
pub fn render_page(slug: &str, sections: &[Section]) -> String {
    let releases_url = format!("https://github.com/{slug}/releases");
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("<meta charset=\"utf-8\" />\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />\n");
    out.push_str("<title>Releases — ALab</title>\n");
    out.push_str(&format!(
        "<meta name=\"description\" content=\"Release notes for aterm, the ALab terminal: the \
         newest releases, rendered from the notes published with each release on github.com/{}.\" />\n",
        esc(slug)
    ));
    out.push_str("<link rel=\"canonical\" href=\"https://alab.systems/releases\" />\n");
    out.push_str(
        "<meta name=\"theme-color\" content=\"#F5F3EC\" media=\"(prefers-color-scheme: light)\" />\n",
    );
    out.push_str(
        "<meta name=\"theme-color\" content=\"#0C0D0C\" media=\"(prefers-color-scheme: dark)\" />\n",
    );
    out.push_str("<meta property=\"og:title\" content=\"aterm releases\" />\n");
    out.push_str(
        "<meta property=\"og:description\" content=\"Release notes for aterm, the terminal \
         written by ALab AI for AI.\" />\n",
    );
    out.push_str("<meta property=\"og:type\" content=\"website\" />\n");
    out.push_str("<meta property=\"og:url\" content=\"https://alab.systems/releases\" />\n");
    out.push_str(
        "<meta property=\"og:image\" content=\"https://alab.systems/assets/og-image.png\" />\n",
    );
    out.push_str("<meta property=\"og:site_name\" content=\"ALab\" />\n");
    out.push_str("<link rel=\"icon\" href=\"/assets/favicon.svg\" type=\"image/svg+xml\" />\n");
    out.push_str("<link rel=\"icon\" href=\"/favicon.ico\" sizes=\"32x32\" />\n");
    out.push_str("<link rel=\"apple-touch-icon\" href=\"/assets/apple-touch-icon.png\" />\n");
    out.push_str("<link rel=\"manifest\" href=\"/site.webmanifest\" />\n");
    out.push_str("<link rel=\"stylesheet\" href=\"/assets/styles.css\" />\n");
    out.push_str("<link rel=\"stylesheet\" href=\"/assets/releases.css\" />\n");
    out.push_str("</head>\n<body>\n<main>\n  <div class=\"card\">\n");
    out.push_str(
        "    <p class=\"wordmark\"><a href=\"/\"><span class=\"bar\" \
         aria-hidden=\"true\">&#9612;</span>ALab</a></p>\n\n",
    );
    out.push_str("    <h1>Releases</h1>\n");
    out.push_str(&format!(
        "    <p class=\"what\">Release notes for <a href=\"/aterm\">aterm</a>, newest first. Each \
         entry links its tag;\n      the current download is on <a href=\"/aterm\">/aterm</a>, \
         and every release with its assets and\n      <code>.sha256</code> sidecars is on <a \
         href=\"{}\">github.com/{}/releases</a>.</p>\n",
        esc(&releases_url),
        esc(slug)
    ));
    if sections.is_empty() {
        out.push_str("    <p class=\"install-note\">No release notes yet.</p>\n");
    } else {
        out.push_str("    <nav class=\"releases-index\" aria-label=\"releases on this page\">");
        let index: Vec<String> = sections
            .iter()
            .map(|s| format!("<a href=\"#{0}\">{0}</a>", esc(&s.tag)))
            .collect();
        out.push_str(&index.join(" &middot; "));
        out.push_str("</nav>\n");
    }
    for s in sections {
        let tag_url = format!("https://github.com/{slug}/releases/tag/{}", s.tag);
        out.push_str(&format!(
            "\n    <section class=\"lane release\" id=\"{}\">\n",
            esc(&s.tag)
        ));
        out.push_str(&format!(
            "      <h2><a href=\"{}\">{}</a><span class=\"release-date\">{}</span></h2>\n",
            esc(&tag_url),
            esc(&s.tag),
            esc(&s.date)
        ));
        let body = if s.body.trim().is_empty() {
            "<p>No notes were written for this release.</p>".to_string()
        } else {
            render_markdown(&s.body)
        };
        out.push_str("      <div class=\"release-notes\">\n");
        let mut in_pre = false;
        for line in body.split('\n') {
            // indented for reading, except inside <pre>, where every byte is content
            if !in_pre {
                out.push_str("        ");
            }
            out.push_str(line);
            out.push('\n');
            in_pre = (in_pre || line.contains("<pre>")) && !line.contains("</pre>");
        }
        out.push_str("      </div>\n    </section>\n");
    }
    out.push('\n');
    out.push_str(SOCIAL_FOOTER);
    out.push_str("  </div>\n</main>\n</body>\n</html>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::releases::parse_releases;
    use std::path::{Path, PathBuf};

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/site")
            .join(name)
    }

    fn read(name: &str) -> String {
        std::fs::read_to_string(fixture(name)).unwrap()
    }

    fn changelog_page() -> String {
        let mut sections = from_changelog(&read("CHANGELOG.md"));
        sections.truncate(LIMIT);
        render_page("alabsystems/aterm", &sections)
    }

    /// The reviewed bytes: same input, same page.
    #[test]
    fn the_changelog_page_is_the_reviewed_golden() {
        assert_eq!(changelog_page(), read("releases.golden.html"));
    }

    #[test]
    fn the_release_bodies_page_is_the_reviewed_golden() {
        let releases = parse_releases(&read("releases-fat.json")).unwrap();
        let mut sections = from_releases(&releases);
        sections.truncate(LIMIT);
        assert_eq!(
            sections.iter().map(|s| s.tag.as_str()).collect::<Vec<_>>(),
            ["v0.63.0", "v0.61.0", "v0.60.0"],
            "stable v-tags only: no prerelease, no package index"
        );
        assert_eq!(
            render_page("alabsystems/aterm", &sections),
            read("releases-bodies.golden.html")
        );
    }

    #[test]
    fn twelve_sections_newest_first_and_never_unreleased() {
        let sections = from_changelog(&read("CHANGELOG.md"));
        assert_eq!(sections.first().map(|s| s.tag.as_str()), Some("v0.64.0"));
        assert_eq!(
            sections.get(LIMIT - 1).map(|s| s.tag.as_str()),
            Some("v0.53.0")
        );
        assert!(
            sections
                .iter()
                .all(|s| !s.body.contains("must never appear"))
        );
        assert_eq!(
            sections.iter().find(|s| s.tag == "v0.62.0").unwrap().body,
            ""
        );
    }

    #[test]
    fn the_markdown_subset_renders_and_everything_else_is_text() {
        let html = |md: &str| render_markdown(md);
        assert_eq!(
            html("raw <b>tag</b> & *emph*"),
            "<p>raw &lt;b&gt;tag&lt;/b&gt; &amp; *emph*</p>"
        );
        assert_eq!(
            html("**`k = \"v\"` draws** and `**not bold**`"),
            "<p><strong><code>k = &quot;v&quot;</code> draws</strong> and <code>**not bold**</code></p>"
        );
        assert_eq!(
            html("[the notes](https://example.invalid/a#b) & https://example.invalid/bare"),
            "<p><a href=\"https://example.invalid/a#b\">the notes</a> &amp; https://example.invalid/bare</p>"
        );
        assert_eq!(
            html("1. one\n2. two `c`"),
            "<ol><li>one</li><li>two <code>c</code></li></ol>"
        );
        assert_eq!(
            html("- a\n  continued\n  - nested\n- b"),
            "<ul><li>a continued<ul><li>nested</li></ul></li><li>b</li></ul>"
        );
        assert_eq!(html("> quoted"), "<blockquote><p>quoted</p></blockquote>");
        assert_eq!(
            html("```\n$ a <b>\n```"),
            "<pre><code>$ a &lt;b&gt;</code></pre>"
        );
        assert_eq!(html("### Added ##\n---"), "<h3>Added</h3>\n<hr />");
        assert_eq!(html("#### Deep"), "<h4>Deep</h4>");
    }

    #[test]
    fn the_cutters_asset_legend_is_dropped_only_in_its_exact_shape() {
        let body = "**aterm** is the terminal. New here? What each file is:\n\n- `a`\n\n---\n\n### Fixed\n";
        assert_eq!(strip_asset_preamble(body), "### Fixed");
        let notes = "### Fixed\n\n---\n\nmore";
        assert_eq!(strip_asset_preamble(notes), notes);
    }
}
