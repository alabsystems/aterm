// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The reader's type at the default size.
const TYPE: ReaderType = ReaderType {
    scale: 1.0,
    display: 24.0,
    title: 20.0,
    body: 13.0,
    caption: 11.0,
};
use aterm_models_native::{native_markdown_history_model, native_markdown_viewport_model};
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, admits};

#[test]
fn parses_blocks_outline_links_images_and_stable_ranges() {
    let source = "# Hello World\n\nText [site](https://example.com).\n\n- one\n\n```rust\nfn x() {}\n```\n\n![diagram](local.png)\n";
    let document = parse(source);
    assert_eq!(document.outline[0].id, "hello-world");
    assert!(matches!(
        document.blocks[0],
        MarkdownBlock::Heading { level: 1, .. }
    ));
    assert!(document
        .blocks
        .iter()
        .any(|block| matches!(block, MarkdownBlock::CodeBlock { language: Some(lang), .. } if lang == "rust")));
    assert_eq!(document.links[0].policy, LinkPolicy::ExplicitExternal);
    assert_eq!(document.images[0].alt.as_deref(), Some("diagram"));
    for block in &document.blocks {
        assert!(block.source().end <= source.len());
        assert!(block.source().start < block.source().end);
    }
}

#[test]
fn reader_hides_comments_but_preserves_code_and_canonical_ranges() {
    let source = "<!-- hidden [bad](https://bad.example) -->\n# **Visible**\n\nBefore <!-- private --> after.\n\n`<!-- inline code -->`\n\n```md\n<!-- fenced code -->\n```\n\n<!-- unclosed";
    let document = parse(source);
    let painted = document
        .blocks
        .iter()
        .map(block_paint_text)
        .collect::<Vec<_>>();

    assert_eq!(
        painted,
        vec![
            "Visible",
            "Before after.",
            "<!-- inline code -->",
            "<!-- fenced code -->\n",
        ]
    );
    assert!(
        document.links.is_empty(),
        "comment links never become actions"
    );
    assert_eq!(document.source_len, source.len());
    assert!(document.blocks.iter().all(|block| {
        block.source().start < block.source().end && block.source().end <= source.len()
    }));
}

#[test]
fn a_list_item_lowers_wrapped_links_and_styles_from_the_whole_item() {
    let source = "2. **A café\r\n   guide** has a [linked\r\nlazy line](part.md).\r\n  - Child\n    continuation\n3. Sibling\n\nNew paragraph";
    let doc = parse(source);
    let [
        MarkdownBlock::ListItem {
            depth: 0,
            ordinal: Some(2),
            text,
            source: first,
        },
        MarkdownBlock::ListItem {
            depth: 1,
            ordinal: None,
            text: child,
            ..
        },
        MarkdownBlock::ListItem {
            ordinal: Some(3),
            text: sibling,
            ..
        },
        MarkdownBlock::Paragraph {
            text: paragraph, ..
        },
    ] = doc.blocks.as_slice()
    else {
        panic!(
            "wrapped list, nested item, sibling, paragraph: {:?}",
            doc.blocks
        );
    };
    assert_eq!(text, "A café guide has a linked lazy line.");
    assert_eq!(child, "Child continuation");
    assert_eq!(sibling, "Sibling");
    assert_eq!(paragraph, "New paragraph");
    assert_eq!(
        &source[first.clone()],
        source.split_once("  - Child").unwrap().0
    );
    let [link] = doc.links.as_slice() else {
        panic!("one link")
    };
    assert_eq!(link.destination.as_ref(), "part.md");
    assert_eq!(
        &source[link.source.clone()],
        "[linked\r\nlazy line](part.md)"
    );
    let styles = doc.block_styles(0, text.len());
    assert!(
        styles
            .iter()
            .any(|(span, style)| style.strong && &text[span.clone()] == "A café guide")
    );
    assert!(
        styles
            .iter()
            .any(|(span, style)| style.link_index == Some(0)
                && &text[span.clone()] == "linked lazy line")
    );
}

#[test]
fn a_list_continuation_stops_before_another_block() {
    for boundary in [
        "# Heading",
        "> Quote",
        "```rust\nlet x = 1;\n```",
        "| A |\n| --- |",
        "---",
        "- Sibling",
        "  * Child",
        "\nOutside",
    ] {
        let doc = parse(&format!("- First line\n  continuation\n{boundary}"));
        assert_eq!(doc.blocks.len(), 2, "{boundary}");
        assert!(
            matches!(&doc.blocks[0], MarkdownBlock::ListItem { text, .. } if text == "First line continuation"),
            "{boundary}"
        );
    }
    let empty = parse("- \nOutside\n\n- \n  Inside");
    let texts: Vec<_> = empty.blocks.iter().map(block_paint_text).collect();
    assert_eq!(texts, ["", "Outside", "Inside"]);
    assert!(matches!(empty.blocks[1], MarkdownBlock::Paragraph { .. }));
    assert!(matches!(empty.blocks[2], MarkdownBlock::ListItem { .. }));
}

/// A backtick run its block never closes is literal, so a comment after it stays hidden; a
/// span closes at its first unescaped equal run and never past its block, as `parse` reads
/// the blocks; a comment inside a real span is code text, and so is any link in it, on every
/// line the span covers. Where the line scan keeps a comment the readers do not show as code,
/// every comment is hidden.
#[test]
fn only_a_closed_code_span_keeps_a_comment_visible() {
    for source in [
        "> <!-- hidden\n[bad](https://bad.example)\n--> Outside.",
        "Price is `5 <!-- [bad](https://bad.example) -->",
        "Price is `5\n\n<!-- [bad](https://bad.example) -->",
        "`a\\` x` <!-- [bad](https://bad.example) --> `",
        "Price is `5 <!-- [bad](https://bad.example) -->\n- item ` tick",
        "- Price `5 <!-- [bad](https://bad.example) -->\n  - Child ` tick",
        "# Price `5 <!-- [bad](https://bad.example) -->\nnext ` line",
        "> Price `5 <!-- [bad](https://bad.example) -->\nnext ` line",
        // The line scan takes these for code that the readers do not show as code.
        "*x `a* <!-- [bad](https://bad.example) --> b`",
        "| h |\n|---|\n| `x <!-- [bad](https://bad.example) --> |\n| z` |",
        "Price `a <!-- [bad](https://bad.example) -->\nb` |\n|---|<!-- -->",
        "| h |\n|---|\n```|\n<!-- [bad](https://bad.example) -->",
    ] {
        let document = parse(source);
        assert!(
            document.links.is_empty(),
            "{source:?}: {:?}",
            document.links
        );
        for block in &document.blocks {
            assert!(
                !block_paint_text(block).contains("bad.example"),
                "{source:?}: {block:?}"
            );
        }
    }

    for (source, painted) in [
        (
            "`<!-- [x](https://x.example) -->`",
            "<!-- [x](https://x.example) -->",
        ),
        ("- `a\n  <!-- c --> b`", "a <!-- c --> b"),
        ("Price `a\n<!-- c --> b` end", "Price a <!-- c --> b end"),
        ("> `a\n> <!-- c --> b`", "a <!-- c --> b"),
        // A span across lines is code on every line it covers, for the links as well.
        (
            "Price `a <!-- [bad](https://bad.example) -->\nb`",
            "Price a <!-- [bad](https://bad.example) --> b",
        ),
        (
            "Price `a\n<!-- [bad](https://bad.example) --> b`",
            "Price a <!-- [bad](https://bad.example) --> b",
        ),
        (
            "Price `a\n[bad](https://bad.example) b`",
            "Price a [bad](https://bad.example) b",
        ),
        (
            "> Price `a\n> [bad](https://bad.example) b`",
            "Price a [bad](https://bad.example) b",
        ),
    ] {
        let document = parse(source);
        let shown = document
            .blocks
            .iter()
            .map(block_paint_text)
            .collect::<Vec<_>>();
        assert_eq!(shown, [painted], "{source:?}");
        assert!(document.links.is_empty(), "{source:?}");
    }

    // The link the readers find first stays a link; the comment the line scan took for code
    // after it is hidden.
    let document = parse("[x `a](https://x.example) <!-- [bad](https://bad.example) --> b`");
    let destinations = document
        .links
        .iter()
        .map(|link| link.destination.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(destinations, ["https://x.example"]);
    assert_eq!(block_paint_text(&document.blocks[0]), "x `a b`");
}

/// A block lends exactly the links its text shows, both read from one text: a link across a
/// paragraph's or a quote's lines is one, a tag the text hides lends none, and a table cell
/// lends only a link that is whole in it.
#[test]
fn a_block_lends_exactly_the_links_its_text_shows() {
    for (source, painted, labels) in [
        ("[a\nb](https://x.example)", "a b", &["a b"][..]),
        ("> see [a\n> b](https://x.example)", "see a b", &["a b"][..]),
        ("<span [x](https://x.example)> y", "y", &[][..]),
        (
            "| [a | b](https://x.example) |\n|---|---|",
            "[a  │  b](https://x.example)\n",
            &[][..],
        ),
        (
            "| [a](https://x.example) | b |\n|---|---|",
            "a  │  b\n",
            &["a"][..],
        ),
    ] {
        let document = parse(source);
        assert_eq!(block_paint_text(&document.blocks[0]), painted, "{source:?}");
        let lent = document
            .links
            .iter()
            .map(|link| link.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(lent, labels, "{source:?}");
        for link in &document.links {
            assert!(
                document.blocks[0].source().start <= link.source.start
                    && link.source.end <= document.blocks[0].source().end,
                "{source:?}: {link:?}"
            );
            let authored = &source[link.source.clone()];
            assert!(
                authored.starts_with('[') && authored.ends_with("(https://x.example)"),
                "{source:?}: {authored:?}"
            );
        }
    }
}

#[test]
fn inline_lowering_presents_authored_text_and_keeps_malformed_literals() {
    let source = r"**strong** *em* ***both*** **outer *inner* tail** foo_bar_baz 2 * 3 \*literal\* `**code** [x](y) &amp;` [**site**](https://example.com) ![*alt*](pic.png) &amp; &#65; &#x42; &bogus;";
    assert_eq!(
        lower_inline_text_styled(source).0,
        "strong em both outer inner tail foo_bar_baz 2 * 3 *literal* **code** [x](y) &amp; site alt & A B &bogus;"
    );

    let document = parse("[**site**](https://example.com) ![*alt*](pic.png)");
    assert_eq!(document.blocks.len(), 1);
    assert_eq!(block_paint_text(&document.blocks[0]), "site alt");
    assert_eq!(document.links[0].label, "site");
    assert_eq!(
        document.links[0].destination.as_ref(),
        "https://example.com"
    );
    assert_eq!(document.images[0].alt.as_deref(), Some("alt"));
    assert_eq!(
        &"[**site**](https://example.com) ![*alt*](pic.png)"[document.links[0].source.clone()],
        "[**site**](https://example.com)"
    );
}

#[test]
fn an_inner_link_keeps_its_action_and_deactivates_the_outer_brackets() {
    let source =
        "[outer **[inner](a.md)**][other] and [mail <me@example.org>](outer.md)\n\n[other]: b.md";
    let doc = parse(source);
    let text = block_paint_text(&doc.blocks[0]);
    assert_eq!(
        text,
        "[outer inner]other and [mail me@example.org](outer.md)"
    );
    let sources: Vec<_> = doc
        .links
        .iter()
        .map(|link| &source[link.source.clone()])
        .collect();
    assert_eq!(sources, ["[inner](a.md)", "[other]", "<me@example.org>"]);
    let hits: Vec<_> = doc
        .block_styles(0, text.len())
        .into_iter()
        .filter_map(|(range, style)| {
            let target = &doc.links[style.link_index?].destination;
            Some((&text[range], target.as_ref(), style.strong))
        })
        .collect();
    assert_eq!(
        hits,
        [
            ("inner", "a.md", true),
            ("other", "b.md", false),
            ("me@example.org", "mailto:me@example.org", false),
        ]
    );
}

#[test]
fn links_in_image_descriptions_supply_text_and_the_enclosing_link_stays_live() {
    let source =
        "[![See [manual][guide] or <me@example.org>](badge.png)](index.md)\n\n[guide]: manual.md";
    let doc = parse(source);
    let text = block_paint_text(&doc.blocks[0]);
    assert_eq!(text, "See manual or me@example.org");
    assert_eq!(doc.images.len(), 1);
    assert_eq!(doc.images[0].alt.as_deref(), Some(text.as_ref()));
    assert_eq!(doc.links.len(), 1);
    assert_eq!(doc.links[0].destination.as_ref(), "index.md");
    let styles = doc.block_styles(0, text.len());
    assert_eq!(styles.len(), 1);
    assert_eq!(styles[0].0, 0..text.len());
    assert_eq!(styles[0].1.link_index, Some(0));
}

/// The PAINT styling of a block, recorded by the lowering pass that produced
/// its text: nested constructs merged (`**a *b* c**`'s `b` is strong AND
/// emphasis; `***both***` is both), a repeated word styled where it was
/// authored (not at its first occurrence), and the plain text between
/// constructs left unstyled.
#[test]
fn inline_styles_locate_nested_and_repeated_runs_in_the_display_text() {
    let source =
        "foo **foo** *em* ***both*** `code` ~~gone~~ [site](https://x.example) **a *b* c**";
    let document = parse(source);
    let block = &document.blocks[0];
    let MarkdownBlock::Paragraph { text, .. } = block else {
        panic!("one paragraph: {block:?}");
    };
    assert_eq!(text, "foo foo em both code gone site a b c");
    let styles = document.block_styles(0, text.len());
    let styled: Vec<(&str, InlineStyle)> = styles
        .iter()
        .map(|(span, style)| (&text[span.clone()], *style))
        .collect();
    let strong = InlineStyle {
        strong: true,
        ..InlineStyle::default()
    };
    let emphasis = InlineStyle {
        emphasis: true,
        ..InlineStyle::default()
    };
    assert_eq!(
        styled,
        vec![
            ("foo", strong),
            ("em", emphasis),
            (
                "both",
                InlineStyle {
                    strong: true,
                    emphasis: true,
                    ..InlineStyle::default()
                }
            ),
            (
                "code",
                InlineStyle {
                    code: true,
                    ..InlineStyle::default()
                }
            ),
            (
                "gone",
                InlineStyle {
                    strike: true,
                    ..InlineStyle::default()
                }
            ),
            (
                "site",
                InlineStyle {
                    link: true,
                    link_index: Some(0),
                    ..InlineStyle::default()
                }
            ),
            ("a ", strong),
            (
                "b",
                InlineStyle {
                    strong: true,
                    emphasis: true,
                    ..InlineStyle::default()
                }
            ),
            (" c", strong),
        ]
    );
    assert_eq!(
        styles[0].0,
        4..7,
        "the STRONG foo is the second one, where it was authored"
    );
}

#[test]
fn inline_semantics_and_reading_source_map_retain_exact_authored_ranges() {
    let source = "Before **strong _em_** and `code` [site](https://example.com).";
    let document = parse(source);
    assert_eq!(document.blocks.len(), 1);
    assert_eq!(
        block_paint_text(&document.blocks[0]),
        "Before strong em and code site."
    );
    for run in &document.inline_runs {
        assert!(run.source.start <= run.content_source.start);
        assert!(run.content_source.start <= run.content_source.end);
        assert!(run.content_source.end <= run.source.end);
        assert!(run.source.end <= document.source_len);
    }
    let authored = document
        .inline_runs
        .iter()
        .map(|run| {
            (
                &run.kind,
                &source[run.source.clone()],
                &source[run.content_source.clone()],
            )
        })
        .collect::<Vec<_>>();
    assert!(authored.iter().any(|(kind, raw, inner)| {
        matches!(kind, MarkdownInlineKind::Strong)
            && *raw == "**strong _em_**"
            && *inner == "strong _em_"
    }));
    assert!(authored.iter().any(|(kind, raw, inner)| {
        matches!(kind, MarkdownInlineKind::Emphasis) && *raw == "_em_" && *inner == "em"
    }));
    assert!(authored.iter().any(|(kind, raw, inner)| {
        matches!(kind, MarkdownInlineKind::Code) && *raw == "`code`" && *inner == "code"
    }));
    assert!(authored.iter().any(|(kind, raw, inner)| {
        matches!(kind, MarkdownInlineKind::Link { .. })
            && *raw == "[site](https://example.com)"
            && *inner == "site"
    }));
    let strong = document
        .inline_runs
        .iter()
        .find(|run| matches!(run.kind, MarkdownInlineKind::Strong))
        .expect("strong run");
    let emphasis = document
        .inline_runs
        .iter()
        .find(|run| matches!(run.kind, MarkdownInlineKind::Emphasis))
        .expect("nested emphasis run");
    assert!(
        strong.content_source.start <= emphasis.source.start
            && emphasis.source.end <= strong.content_source.end,
        "nested emphasis run lies inside enclosing strong content range"
    );
}

#[test]
fn image_policy_requires_gesture_and_never_allows_capability_escape() {
    let document = parse(
        "![local](assets/diagram.png) ![remote](https://example.com/x.png) ![bad](../../secret.png)",
    );
    assert!(matches!(
        reduce_image_action(&document.images[0], true),
        MarkdownImageAction::SelectLocalSource { .. }
    ));
    assert!(matches!(
        reduce_image_action(&document.images[1], false),
        MarkdownImageAction::Denied { .. }
    ));
    assert!(matches!(
        reduce_image_action(&document.images[1], true),
        MarkdownImageAction::OpenRemote { .. }
    ));
    assert!(matches!(
        reduce_image_action(&document.images[2], true),
        MarkdownImageAction::Denied { .. }
    ));
    let schemes = parse(
        "![f](file:///etc/passwd) ![j](javascript:alert(1)) ![d](data:image/png;base64,AA==) ![u](\\\\evil\\share\\x.png) ![a](#frag)",
    );
    for img in &schemes.images {
        assert!(
            matches!(
                reduce_image_action(img, true),
                MarkdownImageAction::Denied { .. }
            ),
            "scheme/UNC/anchor image must be denied: {:?}",
            img.source_uri
        );
    }
}

#[test]
fn reference_definitions_in_quotes_and_lists_resolve_across_containers() {
    let source = "π [guide][manual] and [index].\r\n\r\n\
                  > [manual]: guide.md\r\n\
                  > **Quoted** [index].\r\n\r\n\
                  7. [index]:\r\n   <index.md>\r\n   \"Page list\"\r\n   Continue [manual].";
    let doc = parse(source);
    let [
        MarkdownBlock::Paragraph { .. },
        quoted @ MarkdownBlock::Quote { depth: 1, .. },
        MarkdownBlock::ListItem {
            ordinal: Some(7),
            text: item,
            ..
        },
    ] = doc.blocks.as_slice()
    else {
        panic!("expected a paragraph, quote and numbered item");
    };
    let MarkdownBlock::Paragraph { text: quote, .. } = quoted.content() else {
        panic!("quoted paragraph");
    };
    assert_eq!(quote, "Quoted index.");
    assert_eq!(item, "Continue manual.");
    assert!(
        doc.block_styles(1, quote.len())
            .iter()
            .any(|(span, style)| style.strong && &quote[span.clone()] == "Quoted")
    );
    let uses: Vec<_> = doc
        .links
        .iter()
        .map(|link| (&source[link.source.clone()], link.destination.as_ref()))
        .collect();
    assert_eq!(
        uses,
        [
            ("[guide][manual]", "guide.md"),
            ("[index]", "index.md"),
            ("[index]", "index.md"),
            ("[manual]", "guide.md"),
        ]
    );
    assert_eq!(doc.links[2].source.start, source.rfind("[index].").unwrap());
    assert_eq!(doc.links[3].source.start, source.rfind("[manual]").unwrap());
}

#[test]
fn definition_only_containers_keep_their_markers_and_skip_prose_and_code() {
    let source = "[a] [b] [c] [prose] [code] [list-prose]\n\n\
                  > [a]: a.md\n\n\
                  - [b]: b.md\n\n\
                  > Paragraph\n> [prose]: keep.md\n>\n> ~~~text\n>\n> [code]: keep.md\n> ~~~\n>\n> [c]: c.md\n\n\
                  - Paragraph\n  [list-prose]: keep.md";
    let doc = parse(source);
    assert_eq!(
        doc.links
            .iter()
            .map(|link| link.label.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
    assert_eq!(doc.blocks.len(), 6);
    assert_eq!(doc.blocks[1].quote_depth(), 1);
    assert!(
        matches!(doc.blocks[1].content(), MarkdownBlock::Paragraph { text, .. } if text.is_empty())
    );
    assert!(matches!(&doc.blocks[2], MarkdownBlock::ListItem { text, .. } if text.is_empty()));
    assert_eq!(doc.blocks[3].quote_depth(), 1);
    assert!(
        matches!(doc.blocks[3].content(), MarkdownBlock::Paragraph { text, .. }
        if text == "Paragraph [prose]: keep.md")
    );
    assert!(
        matches!(doc.blocks[4].content(), MarkdownBlock::CodeBlock { code, .. }
        if code == "\n[code]: keep.md\n")
    );
    assert!(
        matches!(&doc.blocks[5], MarkdownBlock::ListItem { text, .. }
        if text == "Paragraph [list-prose]: keep.md")
    );
}

#[test]
fn quote_continuations_lower_inline_markup_across_source_lines() {
    let source = "> **A terminal you can read,\n> reason about, and trust.**\n\nAfter.";
    let document = parse(source);

    assert_eq!(document.blocks.len(), 2);
    assert!(matches!(
        document.blocks[0].content(),
        MarkdownBlock::Paragraph { text, source: range }
            if text == "A terminal you can read, reason about, and trust."
                && &source[range.clone()]
                    == "> **A terminal you can read,\n> reason about, and trust.**\n"
    ));
    assert_eq!(
        block_paint_text(&document.blocks[0]),
        "A terminal you can read, reason about, and trust."
    );
    assert!(matches!(
        &document.blocks[1],
        MarkdownBlock::Paragraph { text, .. } if text == "After."
    ));
    assert_eq!(block_paint_text(&document.blocks[1]), "After.");
}

#[test]
fn quote_continuation_counts_shared_levels_across_physical_gaps() {
    let source = "> # Heading\n> - Item\n> | Column |\n> | --- |\n\
                  >> # First nested\n>> [ref]: page.md\n>> - [ref]\n\
                  >\n>> # Second nested\n\n>> # Separate quote\n";
    let doc = parse(source);
    assert_eq!(
        doc.blocks
            .iter()
            .map(MarkdownBlock::quote_depth)
            .collect::<Vec<_>>(),
        [1, 1, 1, 2, 2, 2, 2]
    );
    assert_eq!(
        (0..=doc.blocks.len())
            .map(|i| doc.quote_continuation(i))
            .collect::<Vec<_>>(),
        [0, 1, 1, 1, 2, 1, 0, 0]
    );
    assert_eq!(doc.quote_continuation(usize::MAX), 0);
}

#[test]
fn empty_parent_quote_lines_separate_nested_leaves() {
    let source = ">\n>> first\n>\n>> second\n>\n\n>\n\n> [page]: page.md\n\n[page]";
    let doc = parse(source);
    let leaves: Vec<_> = doc
        .blocks
        .iter()
        .map(|block| {
            (
                block.quote_depth(),
                block_paint_text(block),
                &source[block.source().clone()],
            )
        })
        .collect();
    assert_eq!(
        leaves,
        [
            (2, Cow::Borrowed("first"), ">> first\n"),
            (2, Cow::Borrowed("second"), ">> second\n"),
            (1, Cow::Borrowed(""), ">\n"),
            (1, Cow::Borrowed(""), "> [page]: page.md\n"),
            (0, Cow::Borrowed("page"), "[page]"),
        ]
    );
    assert_eq!(doc.links[0].destination.as_ref(), "page.md");
}

#[test]
fn quoted_ordered_items_keep_ordinals_wrapped_prose_and_source() {
    let source = "> 3. **Check the build.** Read the\n>    résumé before continuing.\n>\n> 4. **Publish.**\n";
    let doc = parse(source);
    assert_eq!(doc.blocks.len(), 2);
    for (i, block) in doc.blocks.iter().enumerate() {
        assert_eq!(block.quote_depth(), 1);
        let MarkdownBlock::ListItem {
            ordinal,
            text,
            depth,
            ..
        } = block.content()
        else {
            panic!("quoted list item")
        };
        assert_eq!(*ordinal, Some(3 + i as u64));
        assert_eq!(*depth, 0);
        let lead = ["Check the build.", "Publish."][i];
        assert!(
            doc.block_styles(i, text.len())
                .iter()
                .any(|(range, style)| style.strong && &text[range.clone()] == lead)
        );
        assert!(source[block.source().clone()].starts_with(&format!("> {}. ", 3 + i)));
    }
    assert_eq!(
        block_paint_text(&doc.blocks[0]),
        "Check the build. Read the résumé before continuing."
    );
}

#[test]
fn quoted_headings_tables_and_references_use_their_physical_offsets() {
    let source = "Préface.\r\n\r\n  > # Plan\r\n\
                  > > | Item | See |\r\n> > |---|---|\r\n> > | π | [Docs][id] |\r\n\
                  >\r\n> [id]:\r\n>   guide.md\r\n>\r\n\
                  > [two\r\n> lines][id] and ![pic](icon.png)\r\n";
    let doc = parse(source);
    assert_eq!(
        doc.blocks
            .iter()
            .map(MarkdownBlock::quote_depth)
            .collect::<Vec<_>>(),
        [0, 1, 2, 1]
    );
    let heading = source.find("  > #").unwrap();
    assert_eq!(doc.outline[0].source_start, heading);
    assert_eq!(local_anchor(&doc, "#plan"), Some(heading));
    assert_eq!(block_at_source(&doc, heading), Some(1));
    assert!(matches!(
        doc.blocks[2].content(),
        MarkdownBlock::Table { .. }
    ));
    assert_eq!(
        doc.links
            .iter()
            .map(|link| (&source[link.source.clone()], link.destination.as_ref()))
            .collect::<Vec<_>>(),
        [
            ("[Docs][id]", "guide.md"),
            ("[two\r\n> lines][id]", "guide.md")
        ]
    );
    assert_eq!(&source[doc.images[0].source.clone()], "![pic](icon.png)");
}

#[test]
fn quote_fences_keep_literal_inner_markers_and_comments() {
    let source = "> > ~~~rust\n> > > <!-- literal -->\n> > [sample]: literal\n> > ~~~\n\
                  > <!-- hidden\n> comment --> Visible.\n\
                  > ~~~\n> unclosed\n\
                  Outside.\n\n~~~text\n> # literal\n~~~";
    let doc = parse(source);
    assert_eq!(doc.blocks.len(), 5);
    assert_eq!(doc.blocks[0].quote_depth(), 2);
    assert!(
        matches!(doc.blocks[0].content(), MarkdownBlock::CodeBlock { code, .. } if code == "> <!-- literal -->\n[sample]: literal\n")
    );
    assert_eq!(block_paint_text(&doc.blocks[1]), "Visible.");
    assert_eq!(block_paint_text(&doc.blocks[2]), "unclosed\n");
    assert_eq!(doc.blocks[3].quote_depth(), 0);
    assert_eq!(block_paint_text(&doc.blocks[4]), "> # literal\n");

    let comment = parse("> <!-- hidden\n[inside](a.md)\n--> Outside [here](b.md).");
    assert_eq!(comment.links.len(), 1);
    assert_eq!(comment.links[0].label, "here");
    assert_eq!(
        block_paint_text(comment.blocks.last().unwrap()),
        "Outside here."
    );
}

#[test]
fn malformed_and_utf8_inline_markup_never_panics_or_eats_unmatched_text() {
    assert_eq!(
        lower_inline_text_styled("🦀 **unterminated").0,
        "🦀 **unterminated"
    );
    assert_eq!(
        lower_inline_text_styled("[label](unterminated").0,
        "[label](unterminated"
    );
    assert_eq!(
        lower_inline_text_styled("`unterminated **code").0,
        "`unterminated **code"
    );
    assert_eq!(lower_inline_text_styled("<kbd>⌘K</kbd>").0, "⌘K");
    assert_eq!(
        lower_inline_text_styled("<https://example.com>").0,
        "https://example.com"
    );
}

#[test]
fn duplicate_headings_get_deterministic_ids() {
    let document = parse("## Same\n## Same\n## !!!\n");
    assert_eq!(document.outline[0].id, "same");
    assert_eq!(document.outline[1].id, "same-2");
    assert_eq!(document.outline[2].id, "section");
}

#[test]
fn unsafe_schemes_are_never_external_open_requests() {
    let document = parse(
        "[ok](https://example.com) [bad](javascript:alert) [local](README.md) [proto](//evil.example/a) [unc](\\\\evil\\share\\a)",
    );
    assert_eq!(document.links[0].policy, LinkPolicy::ExplicitExternal);
    assert_eq!(document.links[1].policy, LinkPolicy::DeniedScheme);
    assert_eq!(document.links[2].policy, LinkPolicy::LocalDocument);
    assert_eq!(document.links[3].policy, LinkPolicy::DeniedScheme);
    assert_eq!(document.links[4].policy, LinkPolicy::DeniedScheme);
}

#[test]
fn history_evicts_only_the_oldest_entry_at_its_fixed_capacity() {
    let mut history = MarkdownHistory::with_capacity(3);
    for anchor in [10, 20, 30, 40] {
        history.visit(MarkdownLocation::new(anchor, 0));
    }
    assert_eq!(
        history.entries,
        [20, 30, 40].map(|anchor| MarkdownLocation::new(anchor, 0))
    );
    assert_eq!(history.current(), Some(MarkdownLocation::new(40, 0)));
    assert_eq!(history.back(), Some(MarkdownLocation::new(30, 0)));
    assert_eq!(history.back(), Some(MarkdownLocation::new(20, 0)));
    assert_eq!(history.forward(), Some(MarkdownLocation::new(30, 0)));
    history.visit(MarkdownLocation::new(35, 2));
    assert_eq!(
        history.entries,
        [
            MarkdownLocation::new(20, 0),
            MarkdownLocation::new(30, 0),
            MarkdownLocation::new(35, 2),
        ]
    );
    assert_eq!(history.current(), Some(MarkdownLocation::new(35, 2)));
    assert!(!history.can_forward());
}

struct RealHistory {
    history: MarkdownHistory,
    visits: i64,
    last_visit_was_branch: i64,
    expected_len: i64,
}

impl RealHistory {
    fn new() -> Self {
        Self {
            history: MarkdownHistory::with_capacity(3),
            visits: 0,
            last_visit_was_branch: 0,
            expected_len: 0,
        }
    }

    fn project(&self, model: &Model) -> State {
        let mut state = model.init_state();
        state.insert("len", self.history.entries.len() as i64);
        state.insert(
            "cursor",
            self.history.cursor.map_or(0, |cursor| cursor as i64 + 1),
        );
        state.insert("visits", self.visits);
        state.insert("last_visit_was_branch", self.last_visit_was_branch);
        state.insert("expected_len", self.expected_len);
        state
    }

    fn visit(&mut self, anchor: usize) {
        let len = self.history.entries.len();
        let cursor = self.history.cursor.map_or(0, |cursor| cursor + 1);
        self.last_visit_was_branch = i64::from(cursor < len);
        self.expected_len = if cursor == 0 {
            1
        } else if cursor < self.history.capacity {
            cursor + 1
        } else {
            self.history.capacity
        } as i64;
        self.history.visit(MarkdownLocation::new(anchor, 0));
        self.visits += 1;
    }

    fn back(&mut self) {
        assert!(self.history.can_back());
        self.history.back();
        self.last_visit_was_branch = 0;
    }

    fn forward(&mut self) {
        assert!(self.history.can_forward());
        self.history.forward();
        self.last_visit_was_branch = 0;
    }

    fn duplicate(&mut self) {
        let current = self.history.current().expect("non-empty history");
        self.history.visit(current);
        self.last_visit_was_branch = 0;
    }
}

fn drive_history(
    model: &Model,
    real: &mut RealHistory,
    action: &'static str,
    operation: impl FnOnce(&mut RealHistory),
) -> (State, State) {
    let before = real.project(model);
    operation(real);
    let after = real.project(model);
    assert_eq!(admits(model, &before, &after), Some(action));
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, &after),
            "real transition violates {}::{}: {after:?}",
            model.name,
            invariant.name,
        );
    }
    (before, after)
}

#[test]
fn real_markdown_history_conforms_to_derived_capacity_and_branch_model() {
    let model = native_markdown_history_model();
    let mut real = RealHistory::new();
    assert_eq!(real.project(&model), model.init_state());

    drive_history(&model, &mut real, "Visit", |real| real.visit(10));
    drive_history(&model, &mut real, "Visit", |real| real.visit(20));
    drive_history(&model, &mut real, "Visit", |real| real.visit(30));
    drive_history(&model, &mut real, "Back", RealHistory::back);
    drive_history(&model, &mut real, "Back", RealHistory::back);
    let before_branch = real.project(&model);
    drive_history(&model, &mut real, "Visit", |real| real.visit(15));
    assert_eq!(
        real.history.entries,
        [MarkdownLocation::new(10, 0), MarkdownLocation::new(15, 0)]
    );
    drive_history(&model, &mut real, "Visit", |real| real.visit(25));
    let before_full_visit = real.project(&model);
    drive_history(&model, &mut real, "Visit", |real| real.visit(35));
    drive_history(&model, &mut real, "Back", RealHistory::back);
    drive_history(&model, &mut real, "Forward", RealHistory::forward);
    drive_history(&model, &mut real, "Duplicate", RealHistory::duplicate);

    let at_newest = real.project(&model);
    assert!(!real.history.can_forward());
    real.history.forward();
    assert_eq!(
        real.project(&model),
        at_newest,
        "forward at the newest entry"
    );
    let mut past_newest = at_newest.clone();
    past_newest.insert("cursor", at_newest["len"] + 1);
    assert_eq!(admits(&model, &at_newest, &past_newest), None);
    assert!(!model.check_invariant("CursorWithinHistory", &past_newest));

    drive_history(&model, &mut real, "Back", RealHistory::back);
    drive_history(&model, &mut real, "Back", RealHistory::back);
    let at_oldest = real.project(&model);
    assert_eq!(at_oldest["cursor"], 1);
    assert!(!real.history.can_back());
    real.history.back();
    assert_eq!(real.project(&model), at_oldest, "back at the oldest entry");
    let mut off_oldest = at_oldest.clone();
    off_oldest.insert("cursor", 0);
    assert_eq!(admits(&model, &at_oldest, &off_oldest), None);
    assert!(!model.check_invariant("EmptyIffNoCursor", &off_oldest));

    let mut unbounded = before_full_visit.clone();
    unbounded.insert("len", 4);
    unbounded.insert("cursor", 4);
    unbounded.insert("visits", before_full_visit["visits"] + 1);
    unbounded.insert("expected_len", 3);
    assert_eq!(admits(&model, &before_full_visit, &unbounded), None);
    assert!(!model.check_invariant("HistoryBounded", &unbounded));

    let mut untrimmed = before_branch.clone();
    untrimmed.insert("len", 3);
    untrimmed.insert("cursor", 3);
    untrimmed.insert("visits", before_branch["visits"] + 1);
    untrimmed.insert("last_visit_was_branch", 1);
    untrimmed.insert("expected_len", 2);
    assert_eq!(admits(&model, &before_branch, &untrimmed), None);
    assert!(!model.check_invariant("ForwardBranchTruncated", &untrimmed));
}

#[test]
fn tables_and_semantic_text_are_structured() {
    let document = parse("| A | B |\n|---|:---:|\n| 1 | 2 |\n");
    assert_eq!(document.blocks.len(), 1);
    assert!(matches!(document.blocks[0], MarkdownBlock::Table { .. }));
    assert_eq!(block_paint_text(&document.blocks[0]), "A  │  B\n1  │  2\n");
}

#[test]
fn narrow_tables_keep_links_in_their_columns_and_honor_alignment() {
    let source = "| Item | Amount |\n|:-|-:|\n| [red\\|blue](guide.md \"Guide\") | 12 |\n| | 3 |";
    let doc = parse(source);
    let MarkdownBlock::Table { table, .. } = &doc.blocks[0] else {
        panic!("table")
    };
    let (text, styles) = table.project(11);
    assert_eq!(doc.links[0].destination.as_ref(), "guide.md");
    assert_eq!(
        text,
        "Item │Amoun\n     │    t\nred|b│   12\nlue  │     \n     │    3\n"
    );
    let label: String = styles
        .iter()
        .filter(|(_, s)| s.link_index == Some(0))
        .map(|(span, _)| &text[span.clone()])
        .collect();
    assert_eq!(label, "red|blue");
}

#[test]
fn underlined_headings_use_outline_and_fragment_navigation() {
    let doc = parse("A *multi*\nline title\n===\n\nDetails\n---\n\n---\n");
    assert_eq!(
        doc.outline
            .iter()
            .map(|h| (h.level, h.id.as_str()))
            .collect::<Vec<_>>(),
        [(1, "a-multi-line-title"), (2, "details")]
    );
    assert_eq!(
        local_anchor(&doc, "#details"),
        Some(doc.blocks[1].source().start)
    );
    assert!(matches!(doc.blocks[2], MarkdownBlock::ThematicBreak { .. }));
}

#[test]
fn source_to_block_mapping_survives_edges() {
    let document = parse("# A\n\nBody\n");
    assert_eq!(block_at_source(&document, 0), Some(0));
    assert_eq!(block_at_source(&document, 7), Some(1));
    assert_eq!(block_at_source(&document, 99), Some(1));
}

#[test]
fn virtual_layout_materializes_a_bounded_window_with_source_anchors() {
    let source = (0..5_000)
        .map(|index| format!("Paragraph {index} with enough words to wrap in a reader.\n\n"))
        .collect::<String>();
    let document = parse(&source);
    let middle = document.blocks[2_500].source().start;
    let layout = layout_visible_blocks(&document, middle, 0, 680.0, 720.0, TYPE);
    assert_eq!(layout.anchor_index, 2_500);
    assert!(
        layout.blocks.len() < 60,
        "layout work stays viewport-bounded"
    );
    assert!(layout.blocks.iter().any(|block| block.index == 2_500));
    assert!(layout.total_height > 100_000.0);
    assert_eq!(
        block_at_source(&document, document.blocks[2_503].source().start),
        Some(2_503)
    );
    assert_eq!(
        block_at_source(&document, document.blocks[2_497].source().start),
        Some(2_497)
    );
    assert!(
        layout_visible_blocks(&document, middle, 0, f32::NAN, 720.0, TYPE)
            .blocks
            .is_empty()
    );
    assert!(
        layout_visible_blocks(&document, middle, 0, 680.0, f32::NAN, TYPE)
            .blocks
            .is_empty()
    );
    let loc = MarkdownLocation::new(middle, 0);
    assert_eq!(
        move_visual_distance(&document, loc, 680.0, TYPE, f32::NAN, 1),
        loc
    );
}

/// The reader's type at `scale` times the default size.
fn type_at(scale: f32) -> ReaderType {
    ReaderType {
        scale,
        display: TYPE.display * scale,
        title: TYPE.title * scale,
        body: TYPE.body * scale,
        caption: TYPE.caption * scale,
    }
}

/// The rows the painter wraps `block` into in a column `width` wide at `ty`.
fn painted_rows(block: &MarkdownBlock, width: f32, ty: ReaderType) -> usize {
    let frame = preview_frame(block, width, ty).expect("a block with text");
    wrap_reader_text(
        &block_text_at_columns(block, frame.columns(width)),
        frame.columns(width),
        0,
        READER_ROW_LIMIT,
        frame.preserve_lines,
    )
    .len()
}

/// A WIDE CHAR RESERVES TWO COLUMNS OF A BLOCK'S ROWS, as the reader's
/// wrap lays it on two ([`reader_columns`]): a Japanese paragraph, a
/// heading and a code line take no fewer rows than Latin text of twice
/// their chars, at every width.
#[test]
fn a_wide_char_reserves_two_columns_of_a_blocks_rows() {
    let cjk = "設定ファイルを読み込めませんでした".repeat(12);
    let latin = "ab".repeat(cjk.chars().count());
    for (wide, narrow) in [
        (cjk.clone(), latin.clone()),
        (format!("# {cjk}"), format!("# {latin}")),
        (format!("```\n{cjk}\n```"), format!("```\n{latin}\n```")),
    ] {
        let (wide, narrow) = (parse(&wide), parse(&narrow));
        for width in [240.0_f32, 250.0, 480.0, 760.0] {
            let (wide_rows, narrow_rows) = (
                block_visual_rows(&wide.blocks[0], width, TYPE),
                block_visual_rows(&narrow.blocks[0], width, TYPE),
            );
            assert!(
                wide_rows >= narrow_rows && narrow_rows > 1,
                "{wide_rows} rows for {narrow_rows} at {width}: {:?}",
                wide.blocks[0]
            );
        }
    }
    assert_eq!(reader_columns('設'), 2);
    assert_eq!(reader_columns('a'), 1);
    assert_eq!(
        reader_columns('\u{301}'),
        1,
        "a combining mark, as a char always was"
    );
}

/// EVERY ROW A BLOCK PAINTS IS A ROW IT IS LAID OUT WITH, at every text
/// size the reader takes (0.85 to 2.0) and every width: a block's
/// measure is the painter's own wrap of the text it paints, so its
/// rectangle holds every row (no row is cut or elided), and moving by
/// rows to the end of the document reaches the last row of the last
/// block. At 2x the rows grow; the fixed 1x estimate this replaced kept
/// the 1x rows, so the painter cut every block and its tail rows could
/// never be scrolled to.
#[test]
fn a_block_lays_out_every_row_it_paints_at_every_text_size() {
    let source = "# 設定ファイルの読み込み\n\n\
        設定ファイルを読み込めませんでした。テーマ「ドラキュラ」は見つかりません。\
        コーヒーブレイク中にアップデートをダウンロードしています、ちょっと待ってください。\n\n\
        ## A second heading that runs long enough to wrap at a narrow width\n\n\
        ### Third\n\n\
        - a list item that wraps across the measure of a phone-width reader\n\
        \x20\x20- a nested one, deeper, that wraps too when the reader is narrow\n\n\
        > A quoted paragraph that also wraps when the reader is narrow enough.\n\n\
        ```rust\nfn main() { println!(\"a long line of code that wraps in a narrow pane\"); }\n```\n\n\
        ```\n設定ファイルを読み込めませんでした。テーマ「ドラキュラ」は見つかりません。\n```\n\n\
        | Column | Another column with words |\n|---|---|\n| 1 | a cell long enough to wrap the table row |\n\n\
        ---\n\n\
        The quick brown fox jumps over the lazy dog, and aarch64-apple-darwin wraps \
        at its spaces as it always did.\n";
    let document = parse(source);
    assert!(document.blocks.len() >= 10, "{:?}", document.blocks);
    for scale in [0.85_f32, 1.0, 1.5, 2.0] {
        let ty = type_at(scale);
        for width in [206.0_f32, 326.0, 480.0, 732.0] {
            for (index, block) in document.blocks.iter().enumerate() {
                let measure = measure_block(block, width, ty);
                let Some(frame) = preview_frame(block, width, ty) else {
                    assert_eq!(measure.rows, 1);
                    continue;
                };
                let painted = painted_rows(block, width, ty);
                assert_eq!(
                    measure.rows, painted,
                    "block {index} at {scale}x and {width}: {block:?}"
                );
                assert!(
                    measure.height - VISUAL_BLOCK_GAP + 0.01
                        >= frame.inset_top + painted as f32 * frame.line_h,
                    "block {index} at {scale}x and {width}: its rectangle holds every row"
                );
                let layout =
                    layout_visible_blocks(&document, block.source().start, 0, width, 100_000.0, ty);
                assert_eq!(layout.blocks[0].index, index);
                assert_eq!(layout.blocks[0].total_visual_rows, painted);
            }
            let start = MarkdownLocation::new(0, 0);
            let end = move_visual_rows(&document, start, width, ty, isize::MAX);
            let last = document.blocks.len() - 1;
            assert_eq!(
                end.visual_row,
                painted_rows(&document.blocks[last], width, ty) - 1,
                "the last row is reachable at {scale}x and {width}"
            );
            let mut walked = start;
            let mut steps = 0usize;
            loop {
                let next = move_visual_rows(&document, walked, width, ty, 1);
                if next == walked {
                    break;
                }
                walked = next;
                steps += 1;
            }
            let total: usize = document
                .blocks
                .iter()
                .map(|block| measure_block(block, width, ty).rows)
                .sum();
            assert_eq!(
                steps + 1,
                total,
                "one wheel row at a time visits every row at {scale}x and {width}"
            );
            assert_eq!(walked, end);
        }
    }
    let paragraph = &document.blocks[1];
    assert!(
        measure_block(paragraph, 326.0, type_at(2.0)).rows
            > measure_block(paragraph, 326.0, TYPE).rows,
        "larger text wraps into more rows"
    );
}

/// A PAGE MOVES BY THE ROWS THAT FILL MOST OF THE VIEWPORT, each counted
/// at its own height: at 2x a page holds about half the rows it holds at
/// 1x, so paging never jumps past rows the reader did not show.
#[test]
fn a_page_moves_by_the_rows_that_fill_the_viewport_at_their_height() {
    let source = (0..400)
        .map(|line| format!("line-{line:04}\n"))
        .collect::<String>();
    let document = parse(&format!("```\n{source}```\n"));
    let start = MarkdownLocation::new(document.blocks[0].source().start, 0);
    for scale in [1.0_f32, 2.0] {
        let ty = type_at(scale);
        let row_h = measure_block(&document.blocks[0], 600.0, ty).row_h;
        let page = move_visual_distance(&document, start, 600.0, ty, 400.0, 1);
        assert_eq!(
            page.visual_row,
            (400.0 / row_h).floor() as usize,
            "a page at {scale}x"
        );
        let back = move_visual_distance(&document, page, 600.0, ty, 400.0, -1);
        assert_eq!(back.visual_row, 0, "a page back at {scale}x");
        assert_eq!(
            move_visual_distance(&document, start, 600.0, ty, 1.0, 1).visual_row,
            1,
            "at least one row"
        );
    }
}

#[test]
fn thousand_line_fence_is_reachable_by_rows_pages_source_lines_and_end() {
    let source = format!(
        "```text\n{}```\n",
        (0..1_000)
            .map(|line| format!("line-{line:04}\n"))
            .collect::<String>()
    );
    let document = parse(&source);
    assert_eq!(document.blocks.len(), 1);
    let width = 680.0;
    let rows = block_visual_rows(&document.blocks[0], width, TYPE);
    assert!(rows >= 1_000, "every authored code line remains reachable");

    let start = MarkdownLocation::new(document.blocks[0].source().start, 0);
    let one_row = move_visual_rows(&document, start, width, TYPE, 1);
    assert_eq!(one_row.visual_row, 1, "wheel motion stays intra-block");

    let mut paged = start;
    for _ in 0..80 {
        paged = move_visual_rows(&document, paged, width, TYPE, 14);
    }
    assert!(
        paged.visual_row >= 900,
        "page motion reaches the fenced tail"
    );
    let visible = layout_visible_blocks(
        &document,
        paged.source_anchor,
        paged.visual_row,
        width,
        320.0,
        TYPE,
    );
    assert_eq!(visible.blocks[0].index, 0);
    assert_eq!(visible.blocks[0].visual_row, paged.visual_row);

    let end = move_visual_rows(&document, start, width, TYPE, isize::MAX);
    assert_eq!(end.visual_row, rows - 1);
    let back = move_visual_rows(&document, end, width, TYPE, -1);
    assert_eq!(back.visual_row, rows - 2);

    let source_end = move_source_lines(&document, 0, isize::MAX);
    let source_tail = source_window_from_anchor(&source, source_end, 4_096, 32);
    assert!(source_tail.text.contains("line-0999") || source_tail.text.contains("```"));
    assert!(source_tail.truncated_before);
    assert!(!source_tail.text.is_empty());
}

#[test]
fn real_markdown_row_reducer_conforms_to_exact_viewport_model() {
    let document = parse("```text\na\nb\nc\nd\ne\nf\ng\n```\n");
    let model = native_markdown_viewport_model();
    let mut location = MarkdownLocation::new(document.blocks[0].source().start, 0);
    let mut state = model.init_state();
    for step in 1..=6_i64 {
        let before = state.clone();
        location = move_visual_rows(&document, location, 680.0, TYPE, 1);
        state.insert("actual_row", location.visual_row as i64);
        state.insert("expected_row", step);
        state.insert("steps", step);
        assert_eq!(admits(&model, &before, &state), Some("Step"));
        assert!(model.check_invariant("ExactIntraBlockProgress", &state));
    }

    let before = model.init_state();
    let mut skipped = before.clone();
    skipped.insert("actual_row", 4);
    skipped.insert("expected_row", 1);
    skipped.insert("steps", 1);
    assert_eq!(admits(&model, &before, &skipped), None);
    assert!(!model.check_invariant("ExactIntraBlockProgress", &skipped));
}

#[test]
fn decode_entity_rejects_signed_and_null_numeric_entities() {
    let doc = parse("&#65; &#x41; &#+65; &#x+41; &#0; &#x0;\n");
    assert_eq!(doc.blocks.len(), 1);
    assert_eq!(
        doc.blocks[0],
        MarkdownBlock::Paragraph {
            text: "A A &#+65; &#x+41; &#0; &#x0;".to_string(),
            source: 0..39,
        }
    );
}

#[test]
fn four_backtick_fence_preserves_inner_three_backtick_fences_and_masks_comments() {
    let source = "````carousel\n<!-- inner comment inside code fence -->\n```rust\nfn main() {}\n```\n````\n<!--real comment-->\nParagraph\n";
    let doc = parse(source);
    assert_eq!(doc.blocks.len(), 2);
    match &doc.blocks[0] {
        MarkdownBlock::CodeBlock { language, code, .. } => {
            assert_eq!(language.as_deref(), Some("carousel"));
            assert!(code.contains("<!-- inner comment inside code fence -->"));
            assert!(code.contains("```rust\nfn main() {}\n```"));
        }
        other => panic!("expected CodeBlock, got {other:?}"),
    }
    match &doc.blocks[1] {
        MarkdownBlock::Paragraph { text, .. } => assert_eq!(text, "Paragraph"),
        other => panic!("expected Paragraph, got {other:?}"),
    }
}

#[test]
fn inline_triple_backtick_code_span_is_not_treated_as_unclosed_code_fence() {
    let source = "```inline```\n\n<!--comment-->\nNext paragraph\n";
    let doc = parse(source);
    assert_eq!(doc.blocks.len(), 2);
    match &doc.blocks[0] {
        MarkdownBlock::Paragraph { text, .. } => assert_eq!(text, "inline"),
        other => panic!("expected Paragraph, got {other:?}"),
    }
    match &doc.blocks[1] {
        MarkdownBlock::Paragraph { text, .. } => assert_eq!(text, "Next paragraph"),
        other => panic!("expected Paragraph, got {other:?}"),
    }
}

#[test]
fn link_and_image_policies_reject_malformed_and_whitespace_targets() {
    assert_eq!(
        link_policy("https://example.com/a b"),
        LinkPolicy::DeniedScheme
    );
    assert_eq!(link_policy("https://"), LinkPolicy::DeniedScheme);
    assert_eq!(link_policy("https:example.com"), LinkPolicy::DeniedScheme);
    assert_eq!(link_policy("mailto:"), LinkPolicy::DeniedScheme);
    assert_eq!(
        reduce_image_action(
            &MarkdownImage {
                alt: None,
                source_uri: "https://example.com/img.png\n".into(),
                source: 0..10,
                remote: true,
            },
            true,
        ),
        MarkdownImageAction::Denied {
            message: "unsupported or oversized remote image URL",
        }
    );
    assert_eq!(
        reduce_image_action(
            &MarkdownImage {
                alt: None,
                source_uri: "https://".into(),
                source: 0..8,
                remote: true,
            },
            true,
        ),
        MarkdownImageAction::Denied {
            message: "unsupported or oversized remote image URL",
        }
    );
}

/// Image actions always require an explicit user gesture, remote URLs require a non-empty
/// authority, and local image paths reject percent-encoded parent traversal or separators.
/// NEGATIVE CONTROL: omitting `!user_initiated` on local images allows ungestured source
/// selection, and checking only raw `split(['/', '\\'])` admits `..%2fsecret.png` and
/// `https:///etc/passwd`.
#[test]
fn gesture_authority_and_percent_encoded_traversal_guards_hold() {
    let local = MarkdownImage {
        alt: None,
        source_uri: "assets/diagram.png".into(),
        source: 0..20,
        remote: false,
    };
    assert_eq!(
        reduce_image_action(&local, false),
        MarkdownImageAction::Denied {
            message: "image actions require an explicit user gesture",
        }
    );
    for escaped in [
        "..%2f..%2fsecret.png",
        "%2e%2e/secret.png",
        "%2fetc/passwd",
        "%5cevil\\x.png",
        "file%3a///etc/passwd",
        "img%00.png",
        "bad%2",
    ] {
        let candidate = MarkdownImage {
            alt: None,
            source_uri: escaped.into(),
            source: 0..10,
            remote: false,
        };
        assert!(
            matches!(
                reduce_image_action(&candidate, true),
                MarkdownImageAction::Denied { .. }
            ),
            "percent-encoded escape must be denied: {escaped}"
        );
    }
    for empty_host in [
        "https:///etc/passwd",
        "https://?query",
        "https://#frag",
        "https://user@/path",
        "https://:443/path",
        "mailto://evil",
        "/\\evil\\share",
    ] {
        assert_eq!(
            link_policy(empty_host),
            LinkPolicy::DeniedScheme,
            "{empty_host}"
        );
    }
    let remote_empty_host = MarkdownImage {
        alt: None,
        source_uri: "https:///etc/passwd".into(),
        source: 0..20,
        remote: true,
    };
    assert!(matches!(
        reduce_image_action(&remote_empty_host, true),
        MarkdownImageAction::Denied { .. }
    ));
    let doc = parse("<HTTPS://example.com> <https:///etc/passwd>");
    assert_eq!(doc.links.len(), 1);
    assert_eq!(doc.links[0].destination.as_ref(), "HTTPS://example.com");
}

#[test]
fn email_autolinks_keep_the_authored_mailbox_and_source_position() {
    let address = "build+alerts@Linux-2.Example";
    let source = format!("π `<{address}>` or **<{address}>**");
    let document = parse(&source);
    let start = source.rfind('<').unwrap();
    let [link] = document.links.as_slice() else {
        panic!("one actionable address")
    };
    assert_eq!(link.label, address);
    assert_eq!(link.destination.as_ref(), format!("mailto:{address}"));
    assert_eq!(link.policy, LinkPolicy::ExplicitExternal);
    assert_eq!(link.source, start..start + address.len() + 2);
    let run = document
        .inline_runs
        .iter()
        .find(|run| matches!(run.kind, MarkdownInlineKind::Link { index: 0 }))
        .unwrap();
    assert_eq!(run.content_source, start + 1..start + 1 + address.len());
    let text = block_paint_text(&document.blocks[0]);
    assert_eq!(text, format!("π <{address}> or {address}"));
    let styles = document.block_styles(0, text.len());
    let (span, style) = styles
        .iter()
        .find(|(_, style)| style.link_index == Some(0))
        .unwrap();
    assert_eq!(&text[span.clone()], address);
    assert!(style.strong);
    let doc = parse("<a?b%23@x> <mailto:me@example.org>");
    assert_eq!(doc.links[0].destination.as_ref(), "mailto:a%3Fb%2523@x");
    assert_eq!(doc.links[1].destination.as_ref(), doc.links[1].label);
}

#[test]
fn invalid_email_autolinks_remain_readable_between_real_tags() {
    for address in [
        "name@",
        "name@@host",
        "name@x..y",
        "name@-x",
        "name@x-",
        "name@x_y",
        "name@例.jp",
        "name@two hosts",
    ] {
        let doc = parse(&format!("<kbd><{address}></kbd>"));
        assert_eq!(block_paint_text(&doc.blocks[0]), format!("<{address}>"));
        assert!(doc.links.is_empty());
    }
    for length in [63, 64] {
        let doc = parse(&format!("<x@{}>", "a".repeat(length)));
        assert_eq!(doc.links.len(), usize::from(length == 63));
    }
}
