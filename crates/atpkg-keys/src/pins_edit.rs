// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use crate::fsio::concat;

/// The paper-master anchor's constant name.
pub const MASTER_ANCHOR: &str = "PAPER_MASTER_PUBKEYS";

/// Maximum members in the master rotation window.
pub const MAX_MASTER_MEMBERS: usize = 2;

/// Base64 characters in an Ed25519 public key (32 bytes, one `=` of padding), checked here so
/// a wrong length never reaches the file.
const KEY_B64_LEN: usize = 44;

/// The indent for a freshly opened block, matching `pins.rs`.
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
    /// The members only: a derived impl would add byte offsets no reader of a panic message
    /// can use.
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
    if !key.ends_with('=') {
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

/// What a line inside the bracket block is, when it is something this writer recognises.
enum Entry {
    /// A blank separator line.
    Blank,
    /// A `//` comment — the per-member provenance notes `pins.rs` already carries.
    Comment,
    /// A member: exactly one double-quoted literal and its trailing comma.
    Member(String),
}

/// Classify one entry line, or `None` for a shape this writer will not edit around.
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

/// The leading whitespace of `text`. `trim_start` returns a suffix, so the length difference
/// is exactly the leading run and lands on a char boundary. `text.len() - text.trim().len()`
/// would also count trailing whitespace, cutting into the content or off a char boundary.
fn leading_ws(text: &str) -> &str {
    let n = text.len() - text.trim_start().len();
    &text[..n]
}

/// Iterate `src`'s lines as `(text_without_newline, span_including_newline)`.
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
/// Returns an error if the constant is absent, declared twice, malformed, or unterminated.
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

/// Prints the outcome and members, never the file text: a derived impl would dump all of
/// `pins.rs` into any panic message that formatted a `Result<Edit, _>`.
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
mod tests {
    use super::*;

    /// A fixture constant: the master's two-member ceiling would cramp these shape tests.
    const FIXTURE: &str = "FIXTURE_KEYS";
    const FIXTURE_MAX: usize = 4;

    /// Synthetic 44-character base64 keys, shaped like real anchors so the shape checks pass.
    const K1: &str = "cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=";
    const K2: &str = "bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNdc=";
    const NEW: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    const NEW2: &str = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=";

    /// A miniature `pins.rs` with both real shapes: an empty master anchor and a populated,
    /// commented keyset. Its doc comments are there because preserving them is under test.
    fn fixture() -> String {
        String::from(
            "// Copyright 2026 Andrew Yates\n\
             \n\
             /// The paper master. Empty here, and therefore INERT.\n\
             ///\n\
             /// A long doc comment mentioning PAPER_MASTER_PUBKEYS by name, twice:\n\
             /// PAPER_MASTER_PUBKEYS is a list for the same reason the keyset is.\n\
             pub const PAPER_MASTER_PUBKEYS: &[&str] = &[];\n\
             \n\
             /// The channel keyset. ORDER IS A CONTRACT: index 0 is the head.\n\
             pub const FIXTURE_KEYS: &[&str] = &[\n\
             \x20   // K1 — HEAD: the key this build signs with.\n\
             \x20   \"cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=\",\n\
             \x20   // K2 — accept-only.\n\
             \x20   \"bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNdc=\",\n\
             ];\n\
             \n\
             pub const PKG_ROOT_PUBKEY: &str = \"whatever\";\n",
        )
    }

    fn changed(e: Edit) -> (String, Vec<String>) {
        match e {
            Edit::Changed { text, members } => (text, members),
            Edit::AlreadyPresent { .. } => panic!("expected a change, got AlreadyPresent"),
        }
    }

    #[test]
    fn both_declaration_shapes_are_read_and_the_head_is_index_zero() {
        let src = fixture();
        let master = read_anchor(&src, MASTER_ANCHOR).expect("the empty form parses");
        assert!(master.members.is_empty());
        assert_eq!(master.head(), None);

        let channel = read_anchor(&src, FIXTURE).expect("the open form parses");
        assert_eq!(channel.members, vec![K1.to_string(), K2.to_string()]);
        assert_eq!(channel.head(), Some(K1), "index 0 is the head");
    }

    #[test]
    fn appending_the_same_key_twice_produces_one_entry() {
        let src = fixture();
        let (once, members) = changed(
            append_member(&src, FIXTURE, NEW, &["first pass"], FIXTURE_MAX)
                .expect("the first append plans"),
        );
        assert_eq!(
            members,
            vec![K1.to_string(), K2.to_string(), NEW.to_string()]
        );
        assert_eq!(once.matches(NEW).count(), 1, "one entry after one append");

        let second = append_member(&once, FIXTURE, NEW, &["second pass"], FIXTURE_MAX)
            .expect("the second append plans");
        match second {
            Edit::AlreadyPresent { members } => {
                assert_eq!(
                    members,
                    vec![K1.to_string(), K2.to_string(), NEW.to_string()]
                );
            }
            Edit::Changed { .. } => panic!("a re-append must be a no-op, not a second entry"),
        }
        let after = read_anchor(&once, FIXTURE).unwrap();
        assert_eq!(after.members.iter().filter(|m| *m == NEW).count(), 1);
        assert_eq!(once.matches(NEW).count(), 1);
    }

    #[test]
    fn the_existing_keys_survive_and_the_head_stays_at_index_zero() {
        let src = fixture();
        let (text, _) = changed(append_member(&src, FIXTURE, NEW, &["m3"], FIXTURE_MAX).unwrap());
        let after = read_anchor(&text, FIXTURE).unwrap();
        assert_eq!(
            after.members,
            vec![K1.to_string(), K2.to_string(), NEW.to_string()],
            "the incumbents keep their order and the new key lands at the tail"
        );
        assert_eq!(after.head(), Some(K1), "the head is never reordered");

        let (text2, _) =
            changed(append_member(&text, FIXTURE, NEW2, &["m11"], FIXTURE_MAX).unwrap());
        let after2 = read_anchor(&text2, FIXTURE).unwrap();
        assert_eq!(after2.head(), Some(K1));
        assert_eq!(after2.members.len(), 4);
    }

    #[test]
    fn nothing_outside_the_edited_block_is_disturbed() {
        let src = fixture();
        let (text, _) =
            changed(append_member(&src, FIXTURE, NEW, &["provenance"], FIXTURE_MAX).unwrap());
        let before: Vec<&str> = src.lines().collect();
        let after: Vec<&str> = text.lines().collect();
        assert_eq!(
            after.len(),
            before.len() + 2,
            "one comment line and one key line"
        );
        let mut i = 0usize;
        for line in &before {
            let pos = after[i..]
                .iter()
                .position(|l| l == line)
                .expect("every original line survives, in order");
            i += pos + 1;
        }
        assert!(text.contains("/// The paper master. Empty here, and therefore INERT."));
        assert!(text.contains("/// The channel keyset. ORDER IS A CONTRACT: index 0 is the head."));
        assert!(text.contains("pub const PKG_ROOT_PUBKEY: &str = \"whatever\";"));
        assert!(
            read_anchor(&text, MASTER_ANCHOR)
                .unwrap()
                .members
                .is_empty()
        );
    }

    /// The doc comment names the constant twice, which is why the locator anchors on a
    /// column-0 declaration.
    #[test]
    fn the_empty_form_opens_into_a_one_member_list_without_touching_its_doc() {
        let src = fixture();
        let (text, members) = changed(
            append_member(
                &src,
                MASTER_ANCHOR,
                NEW,
                &["the paper master, armed by setup"],
                MAX_MASTER_MEMBERS,
            )
            .unwrap(),
        );
        assert_eq!(members, vec![NEW.to_string()]);
        assert_eq!(read_anchor(&text, MASTER_ANCHOR).unwrap().members, members);
        assert!(
            text.contains("/// PAPER_MASTER_PUBKEYS is a list for the same reason the keyset is."),
            "the doc comment naming the constant must survive verbatim"
        );
        assert!(!text.contains("= &[];"), "the empty form is gone");
        assert_eq!(
            read_anchor(&text, FIXTURE).unwrap().members,
            vec![K1.to_string(), K2.to_string()]
        );

        let last = "pub const PAPER_MASTER_PUBKEYS: &[&str] = &[];";
        let (text, members) =
            changed(append_member(last, MASTER_ANCHOR, NEW, &[], MAX_MASTER_MEMBERS).unwrap());
        assert_eq!(read_anchor(&text, MASTER_ANCHOR).unwrap().members, members);
        assert!(text.ends_with("];"), "{text:?}");
    }

    /// An indent measured as `text.len() - text.trim().len()` from the start of the line
    /// would count trailing whitespace and slice into the line's content.
    #[test]
    fn trailing_whitespace_on_a_neighbouring_line_does_not_poison_the_indent() {
        let src = fixture()
            .replace(
                "    // K1 — HEAD: the key this build signs with.",
                "    // K1 — HEAD: the key this build signs with.  ",
            )
            .replace(
                "    \"cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=\",",
                "    \"cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=\",   ",
            );
        assert_ne!(
            src,
            fixture(),
            "the fixture must actually carry the whitespace"
        );

        let (text, members) = changed(
            append_member(&src, FIXTURE, NEW, &["m3"], FIXTURE_MAX)
                .expect("a line with trailing whitespace is still a line"),
        );
        assert_eq!(
            members,
            vec![K1.to_string(), K2.to_string(), NEW.to_string()]
        );
        assert!(text.contains("\n    // m3\n"), "{text}");
        let mut entry = String::from("\n    \"");
        entry.push_str(NEW);
        entry.push_str("\",\n");
        assert!(text.contains(&entry), "{text}");
        assert!(
            !text.contains("\"cw//"),
            "no key bytes leaked into the comment: {text}"
        );
        assert_eq!(read_anchor(&text, FIXTURE).unwrap().members, members);
    }

    /// A miscomputed indent offset inside a UTF-8 character would panic, and `pins.rs`
    /// comments are full of em-dashes.
    #[test]
    fn a_multibyte_comment_with_trailing_whitespace_does_not_panic() {
        let src = "pub const FIXTURE_KEYS: &[&str] = &[\n\
             \x20   // é    \n\
             \x20   \"cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=\",\n\
             ];\n";
        let anchor = read_anchor(src, FIXTURE).expect("this must not panic");
        assert_eq!(anchor.members, vec![K1.to_string()]);
        assert_eq!(
            anchor.indent, "    ",
            "the indent is the leading run and nothing else"
        );
        let (text, _) = changed(append_member(src, FIXTURE, NEW, &["m3"], FIXTURE_MAX).unwrap());
        assert!(
            text.contains("    // é    \n"),
            "the comment survives verbatim: {text}"
        );
        assert_eq!(read_anchor(&text, FIXTURE).unwrap().members.len(), 2);
    }

    /// A trust anchor's diff is read by a human deciding whether to arm a fleet; mixed line
    /// endings are noise there.
    #[test]
    fn a_crlf_file_keeps_its_line_endings() {
        let src = "pub const PAPER_MASTER_PUBKEYS: &[&str] = &[];\r\n\
             pub const FIXTURE_KEYS: &[&str] = &[\r\n\
             \x20   \"cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=\",\r\n\
             ];\r\n";
        let (text, _) = changed(append_member(src, FIXTURE, NEW, &["m3"], FIXTURE_MAX).unwrap());
        assert!(text.contains("    // m3\r\n"), "{text:?}");
        assert!(
            !text.contains("// m3\n    \""),
            "no bare LF was spliced in: {text:?}"
        );
        assert_eq!(text.matches('\n').count(), text.matches("\r\n").count());

        let (text, _) = changed(
            append_member(src, MASTER_ANCHOR, NEW2, &["master"], MAX_MASTER_MEMBERS).unwrap(),
        );
        assert_eq!(
            text.matches('\n').count(),
            text.matches("\r\n").count(),
            "{text:?}"
        );
        assert_eq!(
            read_anchor(&text, MASTER_ANCHOR).unwrap().members,
            vec![NEW2.to_string()]
        );
    }

    #[test]
    fn unrecognised_shapes_are_refused_not_guessed_at() {
        let err = read_anchor("// nothing here\n", FIXTURE).unwrap_err();
        assert!(err.contains("no `pub const"), "{err}");
        assert!(err.contains("will not guess"), "{err}");

        let twice = fixture().replace(
            "pub const PKG_ROOT_PUBKEY: &str = \"whatever\";",
            "pub const FIXTURE_KEYS: &[&str] = &[];",
        );
        let err = read_anchor(&twice, FIXTURE).unwrap_err();
        assert!(err.contains("2 declarations"), "{err}");
        let nested = fixture() + "mod nested {\n    pub const FIXTURE_KEYS: &[&str] = &[];\n}\n";
        assert_eq!(
            read_anchor(&nested, FIXTURE).unwrap().members,
            [K1.to_string(), K2.to_string()]
        );

        let arrayed = fixture().replace(
            "pub const FIXTURE_KEYS: &[&str] = &[",
            "pub const FIXTURE_KEYS: &[&str; 2] = &[",
        );
        let err = read_anchor(&arrayed, FIXTURE).unwrap_err();
        assert!(err.contains("refusing to guess"), "{err}");

        let computed = fixture().replace(
            "    \"bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNdc=\",",
            "    SOME_OTHER_CONST,",
        );
        let err = read_anchor(&computed, FIXTURE).unwrap_err();
        assert!(err.contains("does not recognise"), "{err}");
        assert!(err.contains("SOME_OTHER_CONST"), "{err}");

        let no_comma = fixture().replace(
            "    \"bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNdc=\",",
            "    \"bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNdc=\"",
        );
        assert!(read_anchor(&no_comma, FIXTURE).is_err());

        for line in [
            "    \"bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNd\\x41\",",
            "    \"bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNdc=\", \"x\",",
            "    ];\n    \"bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNdc=\",",
        ] {
            let odd = fixture().replace(
                "    \"bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNdc=\",",
                line,
            );
            assert!(read_anchor(&odd, FIXTURE).is_err(), "{line}");
        }

        let swallowed = fixture().replace("];\n", "");
        let err = read_anchor(&swallowed, FIXTURE).unwrap_err();
        assert!(err.contains("does not recognise"), "{err}");
        assert!(err.contains("PKG_ROOT_PUBKEY"), "{err}");

        let unterminated = "pub const FIXTURE_KEYS: &[&str] = &[\n    \"x\",\n";
        let err = read_anchor(unterminated, FIXTURE).unwrap_err();
        assert!(err.contains("never closed"), "{err}");

        assert!(append_member(&computed, FIXTURE, NEW, &[], 4).is_err());
        assert!(append_member(&fixture(), FIXTURE, NEW, &[], 4).is_ok());
    }

    #[test]
    fn an_anchor_holding_an_empty_member_is_refused() {
        let bricked = fixture().replace(
            "    \"cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=\",",
            "    \"\",",
        );
        assert_eq!(read_anchor(&bricked, FIXTURE).unwrap().members[0], "");
        let err = append_member(&bricked, FIXTURE, NEW, &[], 4).unwrap_err();
        assert!(err.contains("empty string"), "{err}");
        assert!(err.contains("brick"), "{err}");
    }

    #[test]
    fn a_malformed_key_is_refused_before_any_edit_is_planned() {
        let src = fixture();
        let truncated = &NEW[..43];
        let err = append_member(&src, FIXTURE, truncated, &[], 4).unwrap_err();
        assert!(err.contains("exactly 44"), "{err}");
        assert!(err.contains("43"), "{err}");

        let unpadded = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        assert_eq!(unpadded.len(), 44, "the fixture must be the right length");
        assert!(append_member(&src, FIXTURE, unpadded, &[], 4).is_err());

        let bad_alphabet = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA \"=";
        assert_eq!(bad_alphabet.len(), 44);
        let err = append_member(&src, FIXTURE, bad_alphabet, &[], 4).unwrap_err();
        assert!(err.contains("non-base64"), "{err}");

        assert!(append_member(&src, FIXTURE, "", &[], 4).is_err());
    }

    /// The writer enforces the pins tests' ceiling, so it never produces a file they reject.
    #[test]
    fn the_keyset_ceiling_is_enforced_by_the_writer() {
        let src = fixture();
        let (one, _) =
            changed(append_member(&src, MASTER_ANCHOR, NEW, &[], MAX_MASTER_MEMBERS).unwrap());
        let (two, _) =
            changed(append_member(&one, MASTER_ANCHOR, NEW2, &[], MAX_MASTER_MEMBERS).unwrap());
        let third = "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC=";
        let err = append_member(&two, MASTER_ANCHOR, third, &[], MAX_MASTER_MEMBERS).unwrap_err();
        assert!(err.contains("ceiling is 2"), "{err}");
        assert!(err.contains("rotation window"), "{err}");
    }

    /// Passes on the intended text, fails where the value is absent, altered or reordered.
    #[test]
    fn verification_catches_a_write_that_did_not_land() {
        let src = fixture();
        let (text, members) =
            changed(append_member(&src, FIXTURE, NEW, &["m3"], FIXTURE_MAX).unwrap());
        verify_members(&text, FIXTURE, &members).expect("the intended write verifies");

        let err = verify_members(&src, FIXTURE, &members).unwrap_err();
        assert!(err.contains("holds 2 keys but 3 were intended"), "{err}");

        let mangled = text.replace(NEW, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB=");
        assert_ne!(mangled, text, "the mutation must actually change the text");
        let err = verify_members(&mangled, FIXTURE, &members).unwrap_err();
        assert!(err.contains("[2]"), "{err}");
        assert!(err.contains("was intended"), "{err}");

        let reordered = {
            let mut m = members.clone();
            m.swap(0, 2);
            m
        };
        assert!(verify_members(&text, FIXTURE, &reordered).is_err());

        let err = verify_members("// gone\n", FIXTURE, &members).unwrap_err();
        assert!(err.contains("could not be read back"), "{err}");

        assert!(append_member(&src, FIXTURE, NEW, &["two\nlines"], FIXTURE_MAX).is_err());
    }

    /// The real `pins.rs` reads back as what the compiler compiled in; if it changes shape,
    /// teach the writer the new shape rather than let it guess.
    #[test]
    fn the_shipped_pins_file_is_a_shape_this_writer_understands() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../aterm-update-core/src/pins.rs"
        );
        let src = String::from_utf8(
            crate::fsio::read_bytes(path).expect("the anchor file is in the tree"),
        )
        .unwrap();
        let master = read_anchor(&src, MASTER_ANCHOR).expect("the master anchor is readable");
        assert_eq!(
            master.members,
            aterm_update_core::pins::PAPER_MASTER_PUBKEYS
                .iter()
                .map(|k| (*k).to_string())
                .collect::<Vec<_>>(),
            "what the writer reads out of the source must be what the compiler compiled in"
        );
    }
}
