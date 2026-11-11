// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use crate::fsio::concat;

/// The paper-master anchor's constant name.
pub const MASTER_ANCHOR: &str = "PAPER_MASTER_PUBKEYS";

/// Maximum members in the master rotation window.
pub const MAX_MASTER_MEMBERS: usize = 2;

const KEY_B64_LEN: usize = 44;

const DEFAULT_INDENT: &str = "    ";

/// The two declaration spellings this writer edits, both legal rustfmt output and both in
/// `pins.rs`. Any other (a `&[&str; N]` array, a one-line non-empty list, a `concat!`) is
/// refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// `pub const NAME: &[&str] = &[];` — the empty, unpinned, single-line form.
    EmptyInline,
    /// `pub const NAME: &[&str] = &[` … `];` — the open, multi-line form.
    Open,
}

/// A half-open byte range in the source.
#[derive(Debug, Clone, Copy)]
struct Span {
    start: usize,
    end: usize,
}

/// A located anchor constant.
pub struct Anchor {
    /// The members in source order; index 0 is the head.
    pub members: Vec<String>,
    form: Form,
    /// The declaration line, including its newline.
    decl: Span,
    /// The `];` terminator line, including its newline. `None` for [`Form::EmptyInline`],
    /// which has no separate terminator.
    terminator: Option<Span>,
    /// The indent existing entries use (by `leading_ws`), reproduced for the appended one so
    /// `git diff` shows only the added lines.
    indent: String,
    /// The file's line ending, reproduced by the appended lines so a CRLF file stays CRLF.
    newline: &'static str,
}

impl std::fmt::Debug for Anchor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Anchor(")?;
        f.write_str(&self.members.join(", "))?;
        f.write_str(")")
    }
}

impl Anchor {
    /// The head member — the current master. `None` only for an empty (unpinned,
    /// inert) anchor. Test-only: the edits read `members` directly.
    #[cfg(test)]
    fn head(&self) -> Option<&str> {
        self.members.first().map(String::as_str)
    }
}

/// Refuse a value that is not a 44-character base64 Ed25519 public key.
pub fn vet_key(key: &str) -> Result<(), String> {
    if key.len() != KEY_B64_LEN {
        return Err(concat(&[
            "refusing to write \"",
            key,
            "\" into a trust anchor: an Ed25519 public key is exactly 44 base64 \
             characters and this is ",
            &key.len().to_string(),
        ]));
    }
    if !key.ends_with('=') || key[..KEY_B64_LEN - 1].contains('=') {
        return Err(concat(&[
            "refusing to write \"",
            key,
            "\" into a trust anchor: a 32-byte base64 key ends with one '=' of padding",
        ]));
    }
    if !key
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
    {
        return Err(concat(&[
            "refusing to write \"",
            key,
            "\" into a trust anchor: it contains a non-base64 character",
        ]));
    }
    Ok(())
}

enum Entry {
    /// A blank separator line.
    Blank,
    /// A `//` comment — the per-member provenance notes `pins.rs` already carries.
    Comment,
    /// A member: exactly one double-quoted literal and its trailing comma.
    Member(String),
}

fn classify(trimmed: &str) -> Option<Entry> {
    if trimmed.is_empty() {
        return Some(Entry::Blank);
    }
    if trimmed.starts_with("//") {
        return Some(Entry::Comment);
    }
    let body = trimmed.strip_suffix(',')?;
    let inner = body.strip_prefix('"')?.strip_suffix('"')?;
    if inner.contains('"') || inner.contains('\\') {
        return None;
    }
    Some(Entry::Member(inner.to_string()))
}

fn leading_ws(text: &str) -> &str {
    let n = text.len() - text.trim_start().len();
    &text[..n]
}

fn lines_with_spans(src: &str) -> Vec<(&str, Span)> {
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < src.len() {
        let rest = &src[start..];
        let end = match rest.find('\n') {
            Some(i) => start + i + 1,
            None => src.len(),
        };
        let text = src[start..end]
            .trim_end_matches('\n')
            .trim_end_matches('\r');
        out.push((text, Span { start, end }));
        start = end;
    }
    out
}

/// Locate the named constant and read its current members.
///
/// # Errors
///
/// Absent, duplicate, unrecognised, or unterminated anchor declaration or body line.
pub fn read_anchor(src: &str, name: &str) -> Result<Anchor, String> {
    let lines = lines_with_spans(src);
    let decl_prefix = concat(&["pub const ", name, ":"]);

    let mut found: Option<usize> = None;
    let mut count = 0usize;
    for (i, (text, _)) in lines.iter().enumerate() {
        if text.starts_with(decl_prefix.as_str()) {
            count += 1;
            if found.is_none() {
                found = Some(i);
            }
        }
    }
    if count == 0 {
        return Err(concat(&[
            "no `pub const ",
            name,
            ": &[&str] = …` declaration at column 0 — this is not the pins.rs this tool \
             knows how to edit, and it will not guess",
        ]));
    }
    if count > 1 {
        return Err(concat(&[
            "found ",
            &count.to_string(),
            " declarations of `",
            name,
            "` — refusing to guess which one is the trust anchor",
        ]));
    }
    let idx = found.unwrap_or(0);
    let (decl_text, decl_span) = lines[idx];
    let newline = if src[decl_span.start..decl_span.end].ends_with("\r\n") {
        "\r\n"
    } else {
        "\n"
    };

    let open = concat(&["pub const ", name, ": &[&str] = &["]);
    let empty = concat(&[open.as_str(), "];"]);
    let trimmed_decl = decl_text.trim_end();
    if trimmed_decl == empty {
        return Ok(Anchor {
            members: Vec::new(),
            form: Form::EmptyInline,
            decl: decl_span,
            terminator: None,
            indent: DEFAULT_INDENT.to_string(),
            newline,
        });
    }
    if trimmed_decl != open {
        return Err(concat(&[
            "the declaration of `",
            name,
            "` is spelled `",
            trimmed_decl,
            "`, which is neither `",
            &open,
            "` nor `",
            &empty,
            "`. This writer edits only those two shapes; refusing to guess",
        ]));
    }

    let mut members = Vec::new();
    let mut indent: Option<String> = None;
    for (text, span) in lines.iter().skip(idx + 1) {
        if *text == "];" {
            return Ok(Anchor {
                members,
                form: Form::Open,
                decl: decl_span,
                terminator: Some(*span),
                indent: indent.unwrap_or_else(|| DEFAULT_INDENT.to_string()),
                newline,
            });
        }
        let trimmed = text.trim();
        match classify(trimmed) {
            Some(Entry::Blank) => {}
            Some(Entry::Comment) => {
                if indent.is_none() {
                    indent = Some(leading_ws(text).to_string());
                }
            }
            Some(Entry::Member(m)) => {
                if indent.is_none() {
                    indent = Some(leading_ws(text).to_string());
                }
                members.push(m);
            }
            None => {
                return Err(concat(&[
                    "the body of `",
                    name,
                    "` contains a line this writer does not recognise: `",
                    text,
                    "`. Entries must be a plain quoted key with its trailing comma, a \
                     `//` comment, or blank; refusing to edit around anything else",
                ]));
            }
        }
    }
    Err(concat(&[
        "`",
        name,
        "` opens a `&[` block that is never closed by a `];` at column 0 — refusing to \
         edit a file this writer cannot parse",
    ]))
}

impl std::fmt::Debug for Edit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyPresent { members } => {
                f.write_str("Edit::AlreadyPresent(")?;
                f.write_str(&members.join(", "))?;
                f.write_str(")")
            }
            Self::Changed { members, .. } => {
                f.write_str("Edit::Changed(")?;
                f.write_str(&members.join(", "))?;
                f.write_str(", text=<elided>)")
            }
        }
    }
}

/// The result of planning an append.
pub enum Edit {
    /// The key is already a member.
    AlreadyPresent { members: Vec<String> },
    /// The edit to write, and the members the file must hold afterwards.
    Changed { text: String, members: Vec<String> },
}

/// Plan an append of `key` to the named anchor, with `comment` lines above it.
pub fn append_member(
    src: &str,
    name: &str,
    key: &str,
    comment: &[&str],
    max_members: usize,
) -> Result<Edit, String> {
    vet_key(key)?;
    let anchor = read_anchor(src, name)?;

    for (i, m) in anchor.members.iter().enumerate() {
        if m.is_empty() {
            return Err(concat(&[
                name,
                "[",
                &i.to_string(),
                "] is the empty string. That is a brick, not an unpinned anchor (use an \
                 empty SLICE to unpin) — refusing to edit an anchor already in an illegal \
                 state",
            ]));
        }
    }

    if anchor.members.iter().any(|m| m == key) {
        return Ok(Edit::AlreadyPresent {
            members: anchor.members,
        });
    }
    if anchor.members.len() + 1 > max_members {
        return Err(concat(&[
            name,
            " already holds ",
            &anchor.members.len().to_string(),
            " keys and its ceiling is ",
            &max_members.to_string(),
            ". A keyset is a rotation window, not an accumulator — retire an outgoing key \
             in a reviewed commit before adding another",
        ]));
    }

    let mut entry = String::new();
    for line in comment {
        if line.contains(['\r', '\n']) {
            return Err("refusing a comment line containing a line break".to_string());
        }
        entry.push_str(&anchor.indent);
        entry.push_str("// ");
        entry.push_str(line);
        entry.push_str(anchor.newline);
    }
    entry.push_str(&anchor.indent);
    entry.push('"');
    entry.push_str(key);
    entry.push_str("\",");
    entry.push_str(anchor.newline);

    let mut text = String::new();
    match anchor.form {
        Form::EmptyInline => {
            let raw = &src[anchor.decl.start..anchor.decl.end];
            let nl = if raw.ends_with('\n') {
                anchor.newline
            } else {
                ""
            };
            text.push_str(&src[..anchor.decl.start]);
            text.push_str("pub const ");
            text.push_str(name);
            text.push_str(": &[&str] = &[");
            text.push_str(anchor.newline);
            text.push_str(&entry);
            text.push_str("];");
            text.push_str(nl);
            text.push_str(&src[anchor.decl.end..]);
        }
        Form::Open => {
            let t = anchor
                .terminator
                .ok_or_else(|| "internal: an open anchor with no terminator span".to_string())?;
            text.push_str(&src[..t.start]);
            text.push_str(&entry);
            text.push_str(&src[t.start..]);
        }
    }

    let mut members = anchor.members;
    members.push(key.to_string());

    verify_members(&text, name, &members).map_err(|e| {
        concat(&[
            "the edit this writer planned for `",
            name,
            "` does not read back as the value it intended (",
            &e,
            "). NOTHING HAS BEEN WRITTEN — the plan was refused before it reached the \
             file. This is a bug in the writer, not in your input; please report the \
             surrounding lines of the anchor file",
        ])
    })?;

    Ok(Edit::Changed { text, members })
}

/// Parse `src` and check the named anchor holds exactly `expected`, in order.
pub fn verify_members(src: &str, name: &str, expected: &[String]) -> Result<(), String> {
    let anchor = read_anchor(src, name)
        .map_err(|e| concat(&["`", name, "` could not be read back: ", &e]))?;
    if anchor.members.len() != expected.len() {
        return Err(concat(&[
            "`",
            name,
            "` holds ",
            &anchor.members.len().to_string(),
            " keys but ",
            &expected.len().to_string(),
            " were intended",
        ]));
    }
    for (i, (got, want)) in anchor.members.iter().zip(expected.iter()).enumerate() {
        if got != want {
            return Err(concat(&[
                "`",
                name,
                "[",
                &i.to_string(),
                "]` is \"",
                got,
                "\" but \"",
                want,
                "\" was intended",
            ]));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "pins_edit_tests.rs"]
mod tests;
