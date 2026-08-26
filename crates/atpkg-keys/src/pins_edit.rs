// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The TRUST-ANCHOR WRITER: a surgical, verified, idempotent edit of one
//! `pub const NAME: &[&str] = &[…];` constant in
//! `crates/aterm-update-core/src/pins.rs`.
//!
//! # Why this exists at all, given what it touches
//!
//! `pins.rs` is the file that decides what every shipped binary trusts. Until now the
//! owner hand-edited it: read 44 base64 characters off a terminal, paste them into the
//! anchor list, hope the paste was exact. That is a transcription step in the one place a
//! transcription error is unrecoverable — a mangled anchor either bricks the channel or,
//! worse, arms it with a key nobody holds. So the transcription is removed, and the
//! removal is only an improvement if the writer is *more* careful than a human, not less.
//!
//! # The five rules this writer obeys, and what each one is defending
//!
//! 1. **NARROW.** Nothing outside the `&[ … ]` bracket block of the ONE named constant is
//!    ever touched. The surrounding doc comments are long, load-bearing, and the only
//!    place the rotation rules are written down; a writer that reflowed them would destroy
//!    more value than it added. The edit is a byte-splice at a located offset, never a
//!    parse-and-re-emit of the file.
//! 2. **ANCHORED, never a regex sweep.** The constant is found by scanning for a line that
//!    is EXACTLY its declaration at column 0. A pattern loose enough to also match the
//!    constant's *name inside its own doc comment* — which appears there repeatedly — is a
//!    pattern that will one day edit a comment instead of a value.
//! 3. **IDEMPOTENT.** A key that is already a member is a no-op ([`Edit::AlreadyPresent`]),
//!    not a second entry. Running `setup` twice, or re-running `join` after an interrupted
//!    run, must converge rather than accumulate: a duplicate member fails
//!    `pins::tests::keyset_has_no_duplicates` and, semantically, records a rotation that
//!    never happened.
//! 4. **ADDITIVE.** Existing members survive, in order, with the new key appended at the
//!    TAIL. Index 0 is a contract (`update_channel_signing_pubkey()`); reordering it
//!    strands every client that has not adopted the new head, and release selection has no
//!    fallback, so that is a permanent wedge rather than a delayed update.
//! 5. **REFUSE RATHER THAN GUESS.** Every shape this module does not recognise exactly —
//!    a missing constant, two declarations of it, an unterminated block, an entry line
//!    that is not a plain quoted literal with its trailing comma, a member that is already
//!    the empty string — is an error naming the line, not a best-effort edit. A silently
//!    mangled anchor file is worse than any manual step, which is the whole reason this
//!    module is written this way and not with a regex.
//!
//! And then, because none of the above proves the bytes actually landed:
//! [`verify_members`] re-reads what was written and checks the value it intended is the
//! value now present. The caller runs it against the file on disk, not against the string
//! it just built, so a truncated or racing write is caught rather than assumed away.
//!
//! # What this module deliberately does NOT do
//!
//! It does not commit, does not stage, and does not know what git is. Arming a trust
//! anchor is a reviewed act: the tool edits the working tree and prints the diff to read.

/// The paper-master anchor's constant name.
pub const MASTER_ANCHOR: &str = "PAPER_MASTER_PUBKEYS";

/// The release-channel keyset's constant name.
pub const CHANNEL_ANCHOR: &str = "UPDATE_CHANNEL_PUBKEYS";

/// The ceiling `pins::tests::keyset_is_bounded` enforces. A keyset is a rotation window,
/// not an accumulator, so the writer refuses to push it past the bound rather than
/// producing an edit whose own test suite rejects it.
pub const MAX_CHANNEL_MEMBERS: usize = 4;

/// The ceiling `pins::tests::the_master_keyset_has_no_empty_members_and_no_duplicates`
/// enforces: a master rotation window holds at most two.
pub const MAX_MASTER_MEMBERS: usize = 2;

/// The exact number of base64 characters an Ed25519 public key encodes to (32 bytes → 44
/// with one `=` of padding). `pins::tests::anchors_are_well_formed_base64_ed25519` asserts
/// it; catching a wrong-length value HERE means it never reaches the file.
const KEY_B64_LEN: usize = 44;

/// The default indent for a freshly opened block: four spaces, matching every other
/// entry in `pins.rs`.
const DEFAULT_INDENT: &str = "    ";

/// Which of the two declaration spellings the constant currently uses.
///
/// Both are legal rustfmt output for the same type and both occur in `pins.rs` today, so
/// both are handled — and NOTHING ELSE is. A third spelling (a `&[&str; N]` array, a
/// single-line non-empty list, a `concat!`) is a shape this writer does not understand,
/// and understanding-by-approximation is exactly the failure mode it exists to avoid.
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

/// A located anchor constant: where it is, what it currently holds, and how it is spelled.
pub struct Anchor {
    /// The members in source order. Index 0 is the head, and the head is a contract.
    pub members: Vec<String>,
    form: Form,
    /// The declaration line, including its newline.
    decl: Span,
    /// The `];` terminator line, including its newline. `None` for [`Form::EmptyInline`],
    /// which has no separate terminator.
    terminator: Option<Span>,
    /// The indent existing entries use, reproduced for the appended one so the edit is
    /// invisible in `git diff` except for the line it adds.
    ///
    /// Computed with [`str::trim_start`] and NOT as "length minus the length of the
    /// two-ended trim". That arithmetic looks equivalent and is not: it adds the TRAILING
    /// whitespace to the slice length while slicing from the START, so one stray space at
    /// the end of a neighbouring line silently moved the cut into the line's content. The
    /// consequence was not cosmetic — the indent became `    "cw` and every appended line
    /// was emitted with those bytes as its prefix, producing a `pins.rs` that is not Rust,
    /// on top of an anchor that had just been armed. On a line with a multibyte character
    /// in the right place it panicked outright, mid-run, on a char boundary.
    indent: String,
    /// The line ending this file uses, reproduced by the appended lines. A file written
    /// with CRLF gets CRLF back; splicing LF lines into it would leave mixed endings in a
    /// trust anchor's diff for no reason.
    newline: &'static str,
}

impl std::fmt::Debug for Anchor {
    /// Hand-written for the same reason as [`Edit`]'s: the members are the interesting
    /// part, and a derived impl would carry byte offsets into a file nobody reading a
    /// panic message has in front of them.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Anchor(")?;
        f.write_str(&self.members.join(", "))?;
        f.write_str(")")
    }
}

impl Anchor {
    /// The head member — the key a build SIGNS with, for [`CHANNEL_ANCHOR`]. `None` only
    /// for an empty (unpinned, inert) anchor.
    #[must_use]
    pub fn head(&self) -> Option<&str> {
        self.members.first().map(String::as_str)
    }
}

/// Build a message by concatenation. Same reason as the CLI's own helper: `format!`
/// expands to inlined unsafe `fmt::Arguments` machinery that Trust cannot model and
/// charges to the caller.
fn cat(parts: &[&str]) -> String {
    let mut s = String::new();
    for p in parts {
        s.push_str(p);
    }
    s
}

/// Refuse a value that is not a base64 Ed25519 public key, BEFORE it can reach the file.
///
/// This is the same check `pins::tests::anchors_are_well_formed_base64_ed25519` runs, done
/// early: a truncated paste that lands in the file fails closed at runtime with no hint
/// that the VALUE, not the state, is wrong — and by then it is committed.
pub fn vet_key(key: &str) -> Result<(), String> {
    if key.len() != KEY_B64_LEN {
        return Err(cat(&[
            "refusing to write \"",
            key,
            "\" into a trust anchor: an Ed25519 public key is exactly 44 base64 \
             characters and this is ",
            &key.len().to_string(),
        ]));
    }
    if !key.ends_with('=') {
        return Err(cat(&[
            "refusing to write \"",
            key,
            "\" into a trust anchor: a 32-byte base64 key ends with one '=' of padding",
        ]));
    }
    if !key
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
    {
        return Err(cat(&[
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
///
/// The TRAILING COMMA is required rather than tolerated. Rust allows the last element to
/// omit it, but an appended entry after a comma-less last member would have to modify that
/// member's line too — and "the edit touches only the line it adds" is a property worth
/// more than accepting a spelling rustfmt never produces here.
///
/// Escapes and inner quotes are refused outright: a base64 key contains neither, so their
/// presence means this is not the literal list this writer thinks it is.
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

/// The leading whitespace of `text`, as a slice of it.
///
/// One line, its own function, because the obvious inline spelling of it
/// (`&text[..text.len() - text.trim().len()]`) is wrong in a way that reads as right — see
/// [`Anchor::indent`] for what that cost. Trimming one end and measuring from that same end
/// is the only arithmetic that holds, and `trim_start` returns a suffix of `text`, so the
/// difference of the lengths is exactly the leading run and always lands on a character
/// boundary.
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
/// Every recognisable failure is named: absent, declared twice, declared in a spelling
/// this writer does not understand, unterminated, or containing an entry line that is not
/// a plain quoted literal. None of them is recoverable by guessing, which is why none of
/// them is guessed at.
pub fn read_anchor(src: &str, name: &str) -> Result<Anchor, String> {
    let lines = lines_with_spans(src);
    let decl_prefix = cat(&["pub const ", name, ":"]);

    // A top-level item starts at column 0. Requiring that is what keeps this from ever
    // matching the constant's own name inside its (very long) doc comment.
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
        return Err(cat(&[
            "no `pub const ",
            name,
            ": &[&str] = …` declaration at column 0 — this is not the pins.rs this tool \
             knows how to edit, and it will not guess",
        ]));
    }
    if count > 1 {
        return Err(cat(&[
            "found ",
            &count.to_string(),
            " declarations of `",
            name,
            "` — refusing to guess which one is the trust anchor",
        ]));
    }
    let idx = found.unwrap_or(0);
    let (decl_text, decl_span) = lines[idx];
    // The file's own line ending, read off the located declaration rather than assumed.
    let newline = if src[decl_span.start..decl_span.end].ends_with("\r\n") {
        "\r\n"
    } else {
        "\n"
    };

    let open = cat(&["pub const ", name, ": &[&str] = &["]);
    let empty = cat(&[open.as_str(), "];"]);
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
        return Err(cat(&[
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

    // The open form: walk to the `];` terminator, classifying every line on the way.
    let mut members = Vec::new();
    let mut indent: Option<String> = None;
    for (text, span) in lines.iter().skip(idx + 1) {
        // A `];` at column 0 closes a top-level item. Anything indented is not it.
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
                return Err(cat(&[
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
    Err(cat(&[
        "`",
        name,
        "` opens a `&[` block that is never closed by a `];` at column 0 — refusing to \
         edit a file this writer cannot parse",
    ]))
}

/// The result of planning an append. Nothing is written by planning.
///
/// `Debug` is hand-written rather than derived, and prints the OUTCOME plus the member
/// list — never the file text. A derived one would dump an entire `pins.rs` into any panic
/// message that formatted a `Result<Edit, _>`, which is noise no failure needs.
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

pub enum Edit {
    /// The key is ALREADY a member — the idempotent outcome. Re-running `setup`, or
    /// finishing an interrupted `join`, lands here rather than adding a second entry.
    AlreadyPresent { members: Vec<String> },
    /// The edit to write, and the members the file must hold afterwards.
    Changed { text: String, members: Vec<String> },
}

/// Plan an APPEND of `key` to the named anchor, with `comment` lines above it.
///
/// Append, never insert and never replace: see rule 4 in the module doc. The head slot is
/// a contract and this function cannot reach it — the only way `key` becomes the head is
/// if the anchor was empty, which is the one case where there is no incumbent to strand.
pub fn append_member(
    src: &str,
    name: &str,
    key: &str,
    comment: &[&str],
    max_members: usize,
) -> Result<Edit, String> {
    vet_key(key)?;
    let anchor = read_anchor(src, name)?;

    // An anchor that is ALREADY in an illegal state is not one to append to. An empty
    // member reads as "armed" to `roster_tier_armed()` / the client's keyset gate while
    // authorizing nobody, so the file is a brick already and adding a good key beside the
    // bad one would hide that rather than fix it.
    for (i, m) in anchor.members.iter().enumerate() {
        if m.is_empty() {
            return Err(cat(&[
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
        return Err(cat(&[
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
            // The one case where a line is REPLACED rather than added to: `= &[];` has no
            // body to insert into, so it becomes the open form carrying one entry. The
            // replacement reproduces the declaration verbatim, so the diff is this line
            // plus the entry and nothing else.
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

    // READ BACK WHAT WAS JUST BUILT, BEFORE OFFERING IT TO A CALLER THAT WILL WRITE IT.
    //
    // [`verify_members`] already checks the file after the write, which is the check that
    // catches a short write or a racing editor. It is the WRONG place to catch a bad plan:
    // by the time it fires, the damaged bytes are on disk and the anchor may already be
    // armed. This is the same parse run against the string while it is still in memory, so
    // a plan this module cannot read back is refused with the working tree untouched.
    // It is what turns the trailing-whitespace indent bug from "the anchor file is now not
    // Rust" into "the run stopped and wrote nothing".
    verify_members(&text, name, &members).map_err(|e| {
        cat(&[
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

/// THE PROOF THE VALUE IS THE VALUE: parse `src` and check the named anchor holds exactly
/// `expected`, in order.
///
/// Building a correct string and then writing it are two different claims, and this
/// function is used to check BOTH — by [`append_member`] against the text it has just
/// built (so a plan that cannot be read back never reaches the file at all) and by the
/// caller against the bytes it has just read back from disk (so a short write, a full
/// filesystem or a concurrent editor is caught rather than assumed away). The messages are
/// therefore phase-neutral: each caller says which of the two it was doing, because
/// "nothing has been written" and "the file on disk may be damaged" are opposite
/// instructions and printing the wrong one is worse than printing neither.
pub fn verify_members(src: &str, name: &str, expected: &[String]) -> Result<(), String> {
    let anchor =
        read_anchor(src, name).map_err(|e| cat(&["`", name, "` could not be read back: ", &e]))?;
    if anchor.members.len() != expected.len() {
        return Err(cat(&[
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
            return Err(cat(&[
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

    /// Obviously synthetic 44-character base64 keys. They are shaped like real anchors
    /// (that is the point — the writer's shape checks must pass) and are used nowhere but
    /// here.
    const K1: &str = "cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=";
    const K2: &str = "bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNdc=";
    const NEW: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    const NEW2: &str = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=";

    /// A miniature `pins.rs` with the two real shapes: an empty master anchor and a
    /// populated, comment-annotated channel keyset. Doc comments included, because
    /// preserving them is a property under test.
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
             pub const UPDATE_CHANNEL_PUBKEYS: &[&str] = &[\n\
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

    /// The two shapes in the real file are both read correctly, and the head is the head.
    #[test]
    fn both_declaration_shapes_are_read_and_the_head_is_index_zero() {
        let src = fixture();
        let master = read_anchor(&src, MASTER_ANCHOR).expect("the empty form parses");
        assert!(master.members.is_empty());
        assert_eq!(master.head(), None);

        let channel = read_anchor(&src, CHANNEL_ANCHOR).expect("the open form parses");
        assert_eq!(channel.members, vec![K1.to_string(), K2.to_string()]);
        assert_eq!(channel.head(), Some(K1), "index 0 is the head");
    }

    /// IDEMPOTENT. Appending the same key twice produces one entry, not two — and the
    /// second call reports `AlreadyPresent` rather than silently rewriting the file.
    #[test]
    fn appending_the_same_key_twice_produces_one_entry() {
        let src = fixture();
        let (once, members) = changed(
            append_member(
                &src,
                CHANNEL_ANCHOR,
                NEW,
                &["first pass"],
                MAX_CHANNEL_MEMBERS,
            )
            .expect("the first append plans"),
        );
        assert_eq!(
            members,
            vec![K1.to_string(), K2.to_string(), NEW.to_string()]
        );
        assert_eq!(once.matches(NEW).count(), 1, "one entry after one append");

        let second = append_member(
            &once,
            CHANNEL_ANCHOR,
            NEW,
            &["second pass"],
            MAX_CHANNEL_MEMBERS,
        )
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
        // ...and re-reading the once-written text still sees exactly one.
        let after = read_anchor(&once, CHANNEL_ANCHOR).unwrap();
        assert_eq!(after.members.iter().filter(|m| *m == NEW).count(), 1);
        assert_eq!(once.matches(NEW).count(), 1);
    }

    /// ADDITIVE, AND THE HEAD SURVIVES IN THE HEAD SLOT. This is the property whose
    /// violation wedges the fleet permanently, so it is asserted directly rather than
    /// inferred from the member count.
    #[test]
    fn the_existing_keys_survive_and_the_head_stays_at_index_zero() {
        let src = fixture();
        let (text, _) = changed(
            append_member(&src, CHANNEL_ANCHOR, NEW, &["m3"], MAX_CHANNEL_MEMBERS).unwrap(),
        );
        let after = read_anchor(&text, CHANNEL_ANCHOR).unwrap();
        assert_eq!(
            after.members,
            vec![K1.to_string(), K2.to_string(), NEW.to_string()],
            "the incumbents keep their order and the new key lands at the tail"
        );
        assert_eq!(after.head(), Some(K1), "the head is never reordered");

        // A second machine appends behind the first, still without disturbing the head.
        let (text2, _) = changed(
            append_member(&text, CHANNEL_ANCHOR, NEW2, &["m11"], MAX_CHANNEL_MEMBERS).unwrap(),
        );
        let after2 = read_anchor(&text2, CHANNEL_ANCHOR).unwrap();
        assert_eq!(after2.head(), Some(K1));
        assert_eq!(after2.members.len(), 4);
    }

    /// The doc comments — the load-bearing part of this file — come through byte-identical,
    /// and so does every line outside the one block that was edited.
    #[test]
    fn nothing_outside_the_edited_block_is_disturbed() {
        let src = fixture();
        let (text, _) = changed(
            append_member(
                &src,
                CHANNEL_ANCHOR,
                NEW,
                &["provenance"],
                MAX_CHANNEL_MEMBERS,
            )
            .unwrap(),
        );
        // Every original line still present, in order, with exactly two lines added.
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
        // The OTHER anchor is untouched.
        assert!(
            read_anchor(&text, MASTER_ANCHOR)
                .unwrap()
                .members
                .is_empty()
        );
    }

    /// The empty single-line form becomes the open form carrying exactly one key, and its
    /// doc comment — which mentions the constant's own name twice — is untouched. That
    /// mention is why the locator anchors on a column-0 declaration.
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
        // The channel anchor, in the same file, is untouched.
        assert_eq!(
            read_anchor(&text, CHANNEL_ANCHOR).unwrap().members,
            vec![K1.to_string(), K2.to_string()]
        );
    }

    /// TRAILING WHITESPACE ON A NEIGHBOURING LINE DOES NOT REACH THE APPENDED ONE.
    ///
    /// The indent used to be `text.len() - text.trim().len()` bytes taken from the START of
    /// the line, so trailing whitespace lengthened the slice into the line's CONTENT. Three
    /// spaces after an existing key made the indent `    "cw`, and every line this writer
    /// emitted carried those bytes as its prefix — a `pins.rs` that is not Rust, written
    /// over a trust anchor, and (in `setup`) discovered only after the anchor had been
    /// armed with a master whose phrase the operator had not yet been shown.
    #[test]
    fn trailing_whitespace_on_a_neighbouring_line_does_not_poison_the_indent() {
        // Trailing whitespace after the comment, after the key, and after the declaration.
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
            append_member(&src, CHANNEL_ANCHOR, NEW, &["m3"], MAX_CHANNEL_MEMBERS)
                .expect("a line with trailing whitespace is still a line"),
        );
        assert_eq!(
            members,
            vec![K1.to_string(), K2.to_string(), NEW.to_string()]
        );
        // The appended lines carry the indent and NOTHING else.
        assert!(text.contains("\n    // m3\n"), "{text}");
        let mut entry = String::from("\n    \"");
        entry.push_str(NEW);
        entry.push_str("\",\n");
        assert!(text.contains(&entry), "{text}");
        assert!(
            !text.contains("\"cw//"),
            "no key bytes leaked into the comment: {text}"
        );
        // And it reads back — which is what the writer's own pre-write check now enforces.
        assert_eq!(read_anchor(&text, CHANNEL_ANCHOR).unwrap().members, members);
    }

    /// A MULTIBYTE CHARACTER PLUS TRAILING WHITESPACE DOES NOT PANIC.
    ///
    /// Same arithmetic, worse symptom: when the miscomputed offset landed inside a UTF-8
    /// character the slice panicked mid-run, on a trust-anchor file, with a master live in
    /// memory. `pins.rs`'s own keyset comments are full of em-dashes.
    #[test]
    fn a_multibyte_comment_with_trailing_whitespace_does_not_panic() {
        let src = "pub const UPDATE_CHANNEL_PUBKEYS: &[&str] = &[\n\
             \x20   // é    \n\
             \x20   \"cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=\",\n\
             ];\n";
        let anchor = read_anchor(src, CHANNEL_ANCHOR).expect("this must not panic");
        assert_eq!(anchor.members, vec![K1.to_string()]);
        assert_eq!(
            anchor.indent, "    ",
            "the indent is the leading run and nothing else"
        );
        let (text, _) =
            changed(append_member(src, CHANNEL_ANCHOR, NEW, &["m3"], MAX_CHANNEL_MEMBERS).unwrap());
        assert!(
            text.contains("    // é    \n"),
            "the comment survives verbatim: {text}"
        );
        assert_eq!(read_anchor(&text, CHANNEL_ANCHOR).unwrap().members.len(), 2);
    }

    /// A CRLF FILE KEEPS ITS CRLF ENDINGS. Cosmetic, but a trust anchor's diff is read by a
    /// human deciding whether to arm a fleet, and mixed line endings in it are noise that
    /// costs attention exactly where attention is scarce.
    #[test]
    fn a_crlf_file_keeps_its_line_endings() {
        let src = "pub const PAPER_MASTER_PUBKEYS: &[&str] = &[];\r\n\
             pub const UPDATE_CHANNEL_PUBKEYS: &[&str] = &[\r\n\
             \x20   \"cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=\",\r\n\
             ];\r\n";
        let (text, _) =
            changed(append_member(src, CHANNEL_ANCHOR, NEW, &["m3"], MAX_CHANNEL_MEMBERS).unwrap());
        assert!(text.contains("    // m3\r\n"), "{text:?}");
        assert!(
            !text.contains("// m3\n    \""),
            "no bare LF was spliced in: {text:?}"
        );
        assert_eq!(text.matches('\n').count(), text.matches("\r\n").count());

        // The empty form converts to the open form in the file's own ending too.
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

    /// REFUSE RATHER THAN GUESS: every unrecognised shape is an error naming what it saw.
    #[test]
    fn unrecognised_shapes_are_refused_not_guessed_at() {
        // Absent.
        let err = read_anchor("// nothing here\n", CHANNEL_ANCHOR).unwrap_err();
        assert!(err.contains("no `pub const"), "{err}");
        assert!(err.contains("will not guess"), "{err}");

        // Declared twice.
        let twice = fixture().replace(
            "pub const PKG_ROOT_PUBKEY: &str = \"whatever\";",
            "pub const UPDATE_CHANNEL_PUBKEYS: &[&str] = &[];",
        );
        let err = read_anchor(&twice, CHANNEL_ANCHOR).unwrap_err();
        assert!(err.contains("2 declarations"), "{err}");

        // A spelling this writer does not know: a fixed-size array.
        let arrayed = fixture().replace(
            "pub const UPDATE_CHANNEL_PUBKEYS: &[&str] = &[",
            "pub const UPDATE_CHANNEL_PUBKEYS: &[&str; 2] = &[",
        );
        let err = read_anchor(&arrayed, CHANNEL_ANCHOR).unwrap_err();
        assert!(err.contains("refusing to guess"), "{err}");

        // An entry that is not a plain quoted literal.
        let computed = fixture().replace(
            "    \"bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNdc=\",",
            "    SOME_OTHER_CONST,",
        );
        let err = read_anchor(&computed, CHANNEL_ANCHOR).unwrap_err();
        assert!(err.contains("does not recognise"), "{err}");
        assert!(err.contains("SOME_OTHER_CONST"), "{err}");

        // A member with no trailing comma: legal Rust, but not a line this writer will
        // silently edit around.
        let no_comma = fixture().replace(
            "    \"bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNdc=\",",
            "    \"bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNdc=\"",
        );
        assert!(read_anchor(&no_comma, CHANNEL_ANCHOR).is_err());

        // A stray ITEM inside the block — the terminator went missing and the next
        // top-level declaration was swallowed. Caught as an unrecognised line, which is
        // the right refusal: the writer has no idea where this list ends.
        let swallowed = fixture().replace("];\n", "");
        let err = read_anchor(&swallowed, CHANNEL_ANCHOR).unwrap_err();
        assert!(err.contains("does not recognise"), "{err}");
        assert!(err.contains("PKG_ROOT_PUBKEY"), "{err}");

        // Genuinely unterminated: the block runs to end of file.
        let unterminated = "pub const UPDATE_CHANNEL_PUBKEYS: &[&str] = &[\n    \"x\",\n";
        let err = read_anchor(unterminated, CHANNEL_ANCHOR).unwrap_err();
        assert!(err.contains("never closed"), "{err}");

        // ...and NONE of these produced an edit. The negative control: the same file
        // WITHOUT the damage plans successfully, so the refusals are about the damage.
        assert!(append_member(&computed, CHANNEL_ANCHOR, NEW, &[], 4).is_err());
        assert!(append_member(&fixture(), CHANNEL_ANCHOR, NEW, &[], 4).is_ok());
    }

    /// An anchor already holding an empty member is a brick; the writer refuses to append
    /// beside it rather than hiding the damage under a good key.
    #[test]
    fn an_anchor_holding_an_empty_member_is_refused() {
        let bricked = fixture().replace(
            "    \"cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=\",",
            "    \"\",",
        );
        // The read still succeeds — the shape is legal — so the refusal is a real decision
        // and not a parse accident.
        assert_eq!(
            read_anchor(&bricked, CHANNEL_ANCHOR).unwrap().members[0],
            ""
        );
        let err = append_member(&bricked, CHANNEL_ANCHOR, NEW, &[], 4).unwrap_err();
        assert!(err.contains("empty string"), "{err}");
        assert!(err.contains("brick"), "{err}");
    }

    /// A malformed key never reaches the file: the length, the padding and the alphabet
    /// are all checked before any edit is planned.
    #[test]
    fn a_malformed_key_is_refused_before_any_edit_is_planned() {
        let src = fixture();
        let truncated = &NEW[..43];
        let err = append_member(&src, CHANNEL_ANCHOR, truncated, &[], 4).unwrap_err();
        assert!(err.contains("exactly 44"), "{err}");
        assert!(err.contains("43"), "{err}");

        let unpadded = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        assert_eq!(unpadded.len(), 44, "the fixture must be the right length");
        assert!(append_member(&src, CHANNEL_ANCHOR, unpadded, &[], 4).is_err());

        let bad_alphabet = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA \"=";
        assert_eq!(bad_alphabet.len(), 44);
        let err = append_member(&src, CHANNEL_ANCHOR, bad_alphabet, &[], 4).unwrap_err();
        assert!(err.contains("non-base64"), "{err}");

        assert!(append_member(&src, CHANNEL_ANCHOR, "", &[], 4).is_err());
    }

    /// The ceiling the pins tests enforce is enforced HERE, so the writer never produces
    /// a file its own test suite rejects.
    #[test]
    fn the_keyset_ceiling_is_enforced_by_the_writer() {
        let src = fixture();
        // Master: one key fits, a second fits, a third does not.
        let (one, _) =
            changed(append_member(&src, MASTER_ANCHOR, NEW, &[], MAX_MASTER_MEMBERS).unwrap());
        let (two, _) =
            changed(append_member(&one, MASTER_ANCHOR, NEW2, &[], MAX_MASTER_MEMBERS).unwrap());
        let third = "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC=";
        let err = append_member(&two, MASTER_ANCHOR, third, &[], MAX_MASTER_MEMBERS).unwrap_err();
        assert!(err.contains("ceiling is 2"), "{err}");
        assert!(err.contains("rotation window"), "{err}");
    }

    /// VERIFY-AFTER-WRITE actually verifies: it passes on the text that was intended, and
    /// FAILS on text where the value is absent, altered, or reordered.
    #[test]
    fn verification_catches_a_write_that_did_not_land() {
        let src = fixture();
        let (text, members) = changed(
            append_member(&src, CHANNEL_ANCHOR, NEW, &["m3"], MAX_CHANNEL_MEMBERS).unwrap(),
        );
        verify_members(&text, CHANNEL_ANCHOR, &members).expect("the intended write verifies");

        // The write never happened (the original file is still on disk).
        let err = verify_members(&src, CHANNEL_ANCHOR, &members).unwrap_err();
        assert!(err.contains("holds 2 keys but 3 were intended"), "{err}");

        // One character of the key was corrupted in flight.
        let mangled = text.replace(NEW, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB=");
        assert_ne!(mangled, text, "the mutation must actually change the text");
        let err = verify_members(&mangled, CHANNEL_ANCHOR, &members).unwrap_err();
        assert!(err.contains("[2]"), "{err}");
        assert!(err.contains("was intended"), "{err}");

        // The head was reordered behind our back.
        let reordered = {
            let mut m = members.clone();
            m.swap(0, 2);
            m
        };
        assert!(verify_members(&text, CHANNEL_ANCHOR, &reordered).is_err());

        // The file was truncated to nothing readable.
        let err = verify_members("// gone\n", CHANNEL_ANCHOR, &members).unwrap_err();
        assert!(err.contains("could not be read back"), "{err}");
    }

    /// The REAL `pins.rs` — not a fixture — is a shape this writer understands, and the
    /// keys it reads out of it are the keys the compiled constant holds. If this fails,
    /// the file moved and the writer must be taught the new shape rather than left to
    /// guess at it.
    #[test]
    fn the_shipped_pins_file_is_a_shape_this_writer_understands() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../aterm-update-core/src/pins.rs"
        );
        let src = std::fs::read_to_string(path).expect("the anchor file is in the tree");
        let channel = read_anchor(&src, CHANNEL_ANCHOR).expect("the channel keyset is readable");
        assert_eq!(
            channel.members,
            aterm_update_core::pins::UPDATE_CHANNEL_PUBKEYS
                .iter()
                .map(|k| (*k).to_string())
                .collect::<Vec<_>>(),
            "what the writer reads out of the source must be what the compiler compiled in"
        );
        let master = read_anchor(&src, MASTER_ANCHOR).expect("the master anchor is readable");
        assert_eq!(
            master.members,
            aterm_update_core::pins::PAPER_MASTER_PUBKEYS
                .iter()
                .map(|k| (*k).to_string())
                .collect::<Vec<_>>()
        );
    }
}
