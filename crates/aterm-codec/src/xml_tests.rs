// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn escaping_covers_the_five_entities() {
    assert_eq!(escape("a&b<c>d\"e'f"), "a&amp;b&lt;c&gt;d&quot;e&apos;f");
    assert_eq!(escape("&amp;"), "&amp;amp;");
    assert_eq!(escape("plain/π"), "plain/π");
    let mut out = String::from("<string>");
    escape_into(&mut out, "x<y");
    assert_eq!(out, "<string>x&lt;y");
}
fn events(doc: &str) -> Result<Vec<Event<'_>>, Error> {
    let mut r = Reader::new(doc);
    let mut out = Vec::new();
    while let Some(e) = r.next_event()? {
        out.push(e);
    }
    Ok(out)
}

fn start<'a>(name: &'a str, empty: bool, attrs: &[(&'a str, &str)]) -> Event<'a> {
    Event::Start(Tag {
        name,
        empty,
        attrs: attrs.iter().map(|(k, v)| (*k, (*v).to_owned())).collect(),
    })
}

fn text(s: &str) -> Event<'static> {
    Event::Text(s.to_owned())
}

/// The five entities and both numeric forms unescape in text and attribute values.
/// NEGATIVE CONTROL: mapping `apos` to `"` in `unescape` turns the text into
/// `<>&""` and the attribute into `"A"`, failing the equality.
#[test]
fn references_unescape_in_text_and_attributes() {
    let got = events("<a v=\"&apos;&#65;&#x42;\">&lt;&gt;&amp;&quot;&apos;&#x1F600;&#233;</a>");
    assert_eq!(
        got.unwrap(),
        [
            start("a", false, &[("v", "'AB")]),
            text("<>&\"'\u{1F600}\u{e9}"),
            Event::End("a"),
        ]
    );
}

/// Unknown entities, surrogates, NUL, out-of-range and malformed references are
/// refused with an offset and no document text. NEGATIVE CONTROL: accepting any
/// name in the entity match (mapping it to U+FFFD) lets `&nbsp;` through and fails
/// `unwrap_err`.
#[test]
fn bad_references_are_refused() {
    for doc in [
        "<a>&nbsp;</a>",
        "<a>&#xD800;</a>",
        "<a>&#0;</a>",
        "<a>&#x110000;</a>",
        "<a>&#;</a>",
        "<a>&#x;</a>",
        "<a>&#12a;</a>",
        "<a>& b</a>",
        "<a v='&secret;'/>",
    ] {
        let err = events(doc).unwrap_err();
        assert!(err.offset() > 0 && err.offset() < doc.len(), "{doc}: {err}");
        let shown = err.to_string();
        assert!(
            shown.starts_with("xml: ") && !shown.contains("secret") && !shown.contains("nbsp"),
            "{shown}"
        );
    }
}

/// CDATA is verbatim text; whitespace-only text is reported; the declaration,
/// comments, processing instructions and a plain DOCTYPE are skipped.
/// NEGATIVE CONTROL: unescaping CDATA contents (`unescape` on its body) refuses
/// the section's `<b>` and fails the `unwrap`.
#[test]
fn cdata_whitespace_and_skipped_constructs() {
    let doc = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE r SYSTEM \"r>.dtd\"><!-- c <x> --><r>\n <?pi x?><![CDATA[<b>&amp;</b>]]></r>";
    assert_eq!(
        events(doc).unwrap(),
        [
            text("\n"),
            start("r", false, &[]),
            text("\n "),
            text("<b>&amp;</b>"),
            Event::End("r"),
        ]
    );
}

/// A self-closing tag is one Start with `empty` set and no End; both quote styles
/// and prefixed names read; `attr` finds by exact name. NEGATIVE CONTROL: pushing a
/// self-closing tag onto the open stack makes the parent's end tag a mismatch and
/// fails the `unwrap`.
#[test]
fn self_closing_tags_and_attributes() {
    let doc = "<x:row r=\"1\" spans='1:2'><x:c r:id = \"A&quot;1\" t='s'/></x:row>";
    let got = events(doc).unwrap();
    assert_eq!(
        got,
        [
            start("x:row", false, &[("r", "1"), ("spans", "1:2")]),
            start("x:c", true, &[("r:id", "A\"1"), ("t", "s")]),
            Event::End("x:row"),
        ]
    );
    let Some(Event::Start(c)) = got.get(1) else {
        panic!("second event is the cell");
    };
    assert_eq!(c.attr("r:id"), Some("A\"1"));
    assert_eq!(c.attr("id"), None);
    assert_eq!(c.attrs().count(), 2);
}

/// Malformed structure is refused: a mismatched or stray end, unclosed elements, an
/// internal DOCTYPE subset, a repeated attribute, an unquoted or `<`-holding value,
/// unterminated constructs. NEGATIVE CONTROL: dropping `end_tag`'s name comparison
/// accepts `<a></b>` and fails `unwrap_err`.
#[test]
fn malformed_structure_is_refused() {
    for (doc, reason) in [
        ("<a></b>", "end tag does not match the open element"),
        ("</a>", "end tag does not match the open element"),
        ("<a><b></b>", "input ends inside an element"),
        (
            "<!DOCTYPE r [<!ENTITY e \"x\">]><r/>",
            "a DOCTYPE internal subset is not supported",
        ),
        ("<a x='1' x=\"2\"/>", "attribute repeated in one tag"),
        ("<a x=1/>", "attribute value is not quoted"),
        ("<a x='<'/>", "'<' in an attribute value"),
        ("<a x='1'y='2'/>", "attributes need whitespace before them"),
        ("<a x/>", "attribute without a value"),
        ("<a", "unterminated tag"),
        ("<!-- x", "unterminated comment"),
        ("<a><![CDATA[x</a>", "unterminated CDATA section"),
        ("<?xml", "unterminated processing instruction"),
        ("< a/>", "expected a name"),
    ] {
        let err = events(doc).unwrap_err();
        assert_eq!(err.reason(), reason, "{doc}");
    }
    let mut r = Reader::new("<a></b>");
    assert!(r.next_event().is_ok());
    let first = r.next_event().unwrap_err();
    assert_eq!(r.next_event(), Err(first), "a spent reader refuses again");
}

/// Nesting is bounded at `MAX_DEPTH` open elements. NEGATIVE CONTROL: dropping the
/// depth check reads the 257-deep document and fails `unwrap_err`.
#[test]
fn nesting_depth_is_bounded() {
    let nest = |n: usize| format!("{}{}", "<a>".repeat(n), "</a>".repeat(n));
    assert!(events(&nest(MAX_DEPTH)).is_ok());
    let err = events(&nest(MAX_DEPTH + 1)).unwrap_err();
    assert_eq!(err.reason(), "elements nest too deeply");
    assert_eq!(err.offset(), MAX_DEPTH * 3);
}
