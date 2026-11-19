// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::{LinkPolicy, MarkdownBlock, MarkdownInlineKind, block_paint_text, parse};
use std::sync::Arc;

#[test]
fn forward_definitions_share_the_first_target_and_preserve_authored_spans() {
    let source = "π [*Download*][ Straße ] or [STRASSE][] and [straße].\n\n\
                  [strasse]: <guide.md#install> \"Installation\"\n\
                  [STRASSE]: other.md\n\n\
                  ![diagram][strasse]\n";
    let doc = parse(source);
    assert_eq!(doc.blocks.len(), 2);
    assert_eq!(
        block_paint_text(&doc.blocks[0]),
        "π Download or STRASSE and straße."
    );
    assert_eq!(
        doc.links
            .iter()
            .map(|l| l.label.as_str())
            .collect::<Vec<_>>(),
        ["Download", "STRASSE", "straße"]
    );
    for (link, syntax) in
        doc.links
            .iter()
            .zip(["[*Download*][ Straße ]", "[STRASSE][]", "[straße]"])
    {
        assert_eq!(&source[link.source.clone()], syntax);
        assert_eq!(link.destination.as_ref(), "guide.md#install");
        assert!(Arc::ptr_eq(&doc.links[0].destination, &link.destination));
        assert_eq!(link.policy, LinkPolicy::LocalDocument);
        let run = doc
            .inline_runs
            .iter()
            .find(|run| {
                matches!(run.kind, MarkdownInlineKind::Link { .. }) && run.source == link.source
            })
            .unwrap();
        assert!(syntax.starts_with(&format!("[{}]", &source[run.content_source.clone()])));
    }
    assert_eq!(doc.images.len(), 1);
    assert_eq!(&source[doc.images[0].source.clone()], "![diagram][strasse]");
    assert!(Arc::ptr_eq(
        &doc.images[0].source_uri,
        &doc.links[0].destination
    ));
}

#[test]
fn reference_definitions_only_come_from_paragraph_starts() {
    let source = "[ok] [fenced] [comment] [prose] [indented] [broken] `[ok]`\n\n\
                  <!--\n[comment]: /bad\n-->\n\n\
                  ~~~text\n[fenced]: /bad\n~~~\n\n\
                  Ordinary paragraph\n[prose]: /bad\n\n\
                  \x20\x20\x20\x20[indented]: /bad\n\n\
                  [broken]: /bad \"unterminated\n\n\
                  [ok]:\n  https://example.org/\n  'its title'\n";
    let doc = parse(source);
    assert_eq!(doc.links.len(), 1);
    assert_eq!(doc.links[0].destination.as_ref(), "https://example.org/");
    assert_eq!(
        block_paint_text(&doc.blocks[0]),
        "ok [fenced] [comment] [prose] [indented] [broken] [ok]"
    );
    assert!(doc.blocks.iter().any(|block| matches!(block,MarkdownBlock::CodeBlock {code,..} if code.contains("[fenced]: /bad"))));
}

#[test]
fn table_reference_and_inline_destination_each_keep_their_link_action() {
    let doc = parse(
        "| [Site](#local) | [**Guide**][docs] |\n|:-|:-|\n| [absent] | [docs][] |\n\n[docs]: https://example.org/docs",
    );
    let MarkdownBlock::Table { table, .. } = &doc.blocks[0] else {
        panic!("table")
    };
    let (paint, styles) = table.project(60);
    assert!(paint.contains("[absent]"));
    assert_eq!(
        doc.links
            .iter()
            .map(|l| l.destination.as_ref())
            .collect::<Vec<_>>(),
        [
            "#local",
            "https://example.org/docs",
            "https://example.org/docs"
        ]
    );
    assert!(
        styles.iter().any(|(span, s)| s.link_index == Some(1)
            && s.strong
            && &paint[span.clone()] == "Guide")
    );
}

#[test]
fn labels_bound_length_and_collapse_internal_ascii_whitespace() {
    let long = "x".repeat(1000);
    let source = format!(
        "[word][two\n words] `[literal]` [{long}]\n\n[Two   Words]: /ok\n[{long}]: /oversized\n"
    );
    let doc = parse(&source);
    assert_eq!(doc.links.len(), 1);
    assert_eq!(doc.links[0].destination.as_ref(), "/ok");
    assert!(block_paint_text(doc.blocks.last().unwrap()).contains("/oversized"));
}

#[test]
fn authored_destinations_decode_before_policy_and_sharing() {
    let source = r#"[guide][g] ![diagram][g] [inline](docs/guide\(v2\).md)
[query](h&#116;tps://example.org/?a=1&amp;b=2) [denied][d]

[g]: docs/guide\(v2\).md
[d]: javascript&#x3A;invalid
"#;
    let doc = parse(source);
    assert_eq!(
        doc.links
            .iter()
            .map(|link| (link.destination.as_ref(), link.policy))
            .collect::<Vec<_>>(),
        [
            ("docs/guide(v2).md", LinkPolicy::LocalDocument),
            ("docs/guide(v2).md", LinkPolicy::LocalDocument),
            ("https://example.org/?a=1&b=2", LinkPolicy::ExplicitExternal),
            ("javascript:invalid", LinkPolicy::DeniedScheme),
        ]
    );
    assert!(Arc::ptr_eq(
        &doc.links[0].destination,
        &doc.images[0].source_uri
    ));
    assert_eq!(
        &source[doc.links[1].source.clone()],
        r#"[inline](docs/guide\(v2\).md)"#
    );
}

#[test]
fn destination_escapes_are_single_pass_and_angle_autolinks_are_literal() {
    let doc = parse(r"[escaped](a\&amp;&#38;amp;&unknown;\q) <https://example.org/?a=1&amp;b=2>");
    assert_eq!(
        doc.links
            .iter()
            .map(|link| link.destination.as_ref())
            .collect::<Vec<_>>(),
        [
            r"a&amp;&amp;&unknown;\q",
            "https://example.org/?a=1&amp;b=2"
        ]
    );
}
