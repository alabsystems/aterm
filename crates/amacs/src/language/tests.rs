// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Every line of `text` lexed from the start, one rendered line per line.
fn render(language: Language, text: &str) -> String {
    let mut state = LexState::START;
    let mut rendered = Vec::new();
    for line in text.split('\n') {
        let (spans, next) = language.lex_line(state, line);
        let parts: Vec<String> = spans
            .iter()
            .map(|span| format!("{}«{}»", letter(span.class), &line[span.range.clone()]))
            .collect();
        rendered.push(parts.join(" "));
        state = next;
    }
    rendered.join("\n")
}

fn letter(class: SyntaxClass) -> char {
    match class {
        SyntaxClass::Keyword => 'K',
        SyntaxClass::String => 'S',
        SyntaxClass::Comment => 'C',
        SyntaxClass::Number => 'N',
        SyntaxClass::Heading => 'H',
        SyntaxClass::Punct => 'P',
        SyntaxClass::Added => 'A',
        SyntaxClass::Removed => 'R',
        SyntaxClass::Meta => 'M',
    }
}

#[test]
fn detect_table() {
    let table: &[(&str, &str, Language)] = &[
        ("/repo/.git/COMMIT_EDITMSG", "", Language::GitCommit),
        (
            "/repo/.git/MERGE_MSG",
            "Merge branch 'x'",
            Language::GitCommit,
        ),
        ("TAG_EDITMSG", "", Language::GitCommit),
        ("/repo/.git/SQUASH_MSG", "", Language::GitCommit),
        ("file:///repo/.git/COMMIT_EDITMSG", "", Language::GitCommit),
        ("C:\\repo\\.git\\COMMIT_EDITMSG", "", Language::GitCommit),
        (
            "/repo/.git/rebase-merge/git-rebase-todo",
            "pick 1a2b3c4 x",
            Language::GitRebaseTodo,
        ),
        ("commit_editmsg", "", Language::Plain),
        ("COMMIT_EDITMSG.bak", "", Language::Plain),
        ("README.md", "", Language::Markdown),
        ("notes.MARKDOWN", "", Language::Markdown),
        ("Cargo.toml", "", Language::Toml),
        ("src/lib.rs", "", Language::Rust),
        ("tools/build.sh", "", Language::Shell),
        ("x.zsh", "", Language::Shell),
        ("fix.patch", "", Language::Diff),
        ("x.diff", "", Language::Diff),
        ("Cargo.lock", "", Language::Toml),
        ("/home/me/.bashrc", "", Language::Shell),
        ("/home/me/.zshrc", "", Language::Shell),
        (".md", "", Language::Plain),
        ("main.rs", "#!/bin/sh", Language::Rust),
        ("tools/install", "#!/bin/sh", Language::Shell),
        ("x", "#!/usr/bin/env bash", Language::Shell),
        ("x", "#! /bin/zsh -f", Language::Shell),
        ("x", "#!/usr/bin/env -S bash -e", Language::Shell),
        (
            "x",
            "#!/usr/bin/env -S cargo +nightly -Zscript",
            Language::Rust,
        ),
        ("x.py", "#!/usr/bin/env python3", Language::Plain),
        ("-", "diff --git a/x b/x", Language::Diff),
        (
            "0001",
            "From 1234567890abcdef1234567890abcdef12345678 Mon Sep 17 00:00:00 2001",
            Language::Diff,
        ),
        ("From", "From me to you", Language::Plain),
        ("notes.txt", "", Language::Plain),
        ("/Users//me/accounts.csv", "computer,email", Language::Csv),
        ("file:///tmp/My%20List.CSV", "", Language::Csv),
        ("export.tsv", "", Language::Tsv),
        ("export.tab", "", Language::Tsv),
        (".csv", "", Language::Plain),
        ("Makefile", "all:", Language::Plain),
        ("", "", Language::Plain),
    ];
    for &(path, first_line, expected) in table {
        assert_eq!(
            Language::detect(path, first_line),
            expected,
            "detect({path:?}, {first_line:?})"
        );
    }
}

#[test]
fn names_labels_and_defaults() {
    for language in Language::ALL {
        assert_eq!(Language::from_name(language.name()), Some(language));
        assert_eq!(
            language.fill_column(),
            if language == Language::GitCommit {
                72
            } else {
                70
            }
        );
    }
    assert_eq!(Language::from_name("GitCommit"), None);
    assert_eq!(Language::GitCommit.label(), "GitCommit");
    assert_eq!(Language::GitCommit.summary_limit(), Some(50));
    assert_eq!(Language::Markdown.summary_limit(), None);
    let labels: std::collections::HashSet<_> = Language::ALL.map(Language::label).into();
    assert_eq!(labels.len(), Language::ALL.len());
    assert_eq!(Language::default(), Language::Plain);
    for language in Language::ALL {
        let expected = match language {
            Language::Csv => Some(crate::table::Delimiter::Comma),
            Language::Tsv => Some(crate::table::Delimiter::Tab),
            _ => None,
        };
        assert_eq!(language.delimiter(), expected, "{language:?}");
    }
}

#[test]
fn comment_syntax_table() {
    let rust = Language::Rust.comment();
    assert_eq!(rust.line, Some("// "));
    assert_eq!(rust.block, Some(("/* ", " */")));
    assert_eq!(
        Language::Markdown.comment(),
        CommentSyntax {
            line: None,
            block: Some(("<!-- ", " -->"))
        }
    );
    for hash in [
        Language::Toml,
        Language::Shell,
        Language::GitCommit,
        Language::GitRebaseTodo,
    ] {
        assert_eq!(hash.comment().line, Some("# "), "{hash:?}");
        assert_eq!(hash.comment().block, None, "{hash:?}");
    }
    for none in [
        Language::Plain,
        Language::Diff,
        Language::Csv,
        Language::Tsv,
    ] {
        assert_eq!(
            none.comment(),
            CommentSyntax {
                line: None,
                block: None
            }
        );
    }
}

#[test]
fn golden_lex_per_language() {
    let rust = r##"#[derive(Debug)]
pub fn add<'a>(x: &'a str) -> u32 {
    let s = "hi\"there"; // note
    /* open /* nested */
       still */ let n = 0x1F_u8 + 1.5e3;
    r#"raw "quoted""#; b'x'; println!("{n}");
    let c = '\n'; let crab = '🦀'; let s = "two
lines";
}"##;
    let rust_expected = r##"M«#[derive» P«(» P«)]»
K«pub» K«fn» P«<» M«'a» P«>(» P«:» P«&» M«'a» P«)» P«->» P«{»
K«let» P«=» S«"hi\"there"» P«;» C«// note»
C«/* open /* nested */»
C«       still */» K«let» P«=» N«0x1F_u8» P«+» N«1.5e3» P«;»
S«r#"raw "quoted""#» P«;» S«b'x'» P«;» M«println!» P«(» S«"{n}"» P«);»
K«let» P«=» S«'\n'» P«;» K«let» P«=» S«'🦀'» P«;» K«let» P«=» S«"two»
S«lines"» P«;»
P«}»"##;
    assert_eq!(render(Language::Rust, rust), rust_expected);

    let toml = r#"# config
[package]
name = "amacs" # trailing
version.workspace = true
"quoted key" = 'lit'
[[bin]]
deps = [
    "a",
    { path = "../x", optional = false },
]
when = 1979-05-27T07:32:00Z
big = 1_000
neg = -inf
text = """
multi "line"
""""#;
    let toml_expected = r#"C«# config»
H«[package]»
K«name» P«=» S«"amacs"» C«# trailing»
K«version» P«.» K«workspace» P«=» K«true»
K«"quoted key"» P«=» S«'lit'»
H«[[bin]]»
K«deps» P«=» P«[»
S«"a"» P«,»
P«{» K«path» P«=» S«"../x"» P«,» K«optional» P«=» K«false» P«},»
P«]»
K«when» P«=» N«1979-05-27T07:32:00Z»
K«big» P«=» N«1_000»
K«neg» P«=» N«-inf»
K«text» P«=» S«"""»
S«multi "line"»
S«"""»"#;
    assert_eq!(render(Language::Toml, toml), toml_expected);

    let shell = r#"#!/bin/sh
# comment
if [ -n "$HOME" ]; then
  echo 'it''s' $USER ${PATH} $1 # tail
fi
FOO=bar cmd 2>&1 | grep -v x#y
msg="open
still""#;
    let shell_expected = r#"C«#!/bin/sh»
C«# comment»
K«if» S«"$HOME"» P«;» K«then»
S«'it''s'» M«$USER» M«${PATH}» M«$1» C«# tail»
K«fi»
M«FOO» N«2» P«>&» N«1» P«|»
M«msg» S«"open»
S«still"»"#;
    assert_eq!(render(Language::Shell, shell), shell_expected);

    let heredoc = "cat <<EOF >out
Don't stop
EOF
if true; then x=$((1 << 2)); fi
\tcat <<- 'END' # tabs
\tit's \"$quoted\"
\tEND\r
done";
    let heredoc_expected = "P«<<» M«EOF» P«>»
S«Don't stop»
M«EOF»
K«if» P«;» K«then» M«x» P«$((» P«<<» N«2» P«));» K«fi»
P«<<-» M«'END'» C«# tabs»
S«\tit's \"$quoted\"»
M«END»
K«done»";
    assert_eq!(render(Language::Shell, heredoc), heredoc_expected);

    let markdown = "# Title\n\
Some `code` and [a link](https://x.y) <https://auto.link>.\n\
> quote\n\
- item\n\
1. first\n\
- [ ] task\n\
---\n\
<!-- a\n\
comment -->\n\
```rust\n\
let x = 1;\n\
```\n\
| a | b |";
    let markdown_expected = "H«# Title»\n\
S«`code`» P«](» M«https://x.y» P«)» M«<https://auto.link>»\n\
P«>»\n\
P«-»\n\
P«1.»\n\
P«-» P«[ ]»\n\
P«---»\n\
C«<!-- a»\n\
C«comment -->»\n\
M«```rust»\n\
S«let x = 1;»\n\
M«```»\n\
P«|» P«|» P«|»";
    assert_eq!(render(Language::Markdown, markdown), markdown_expected);

    let quoted = "> ```\n> x = 1;\n> ```\n# After\n> ~~~\n> y\nplain\n```\n> ```\n```";
    let quoted_expected = "P«>» M«```»\n\
P«>» S«x = 1;»\n\
P«>» M«```»\n\
H«# After»\n\
P«>» M«~~~»\n\
P«>» S«y»\n\
\n\
M«```»\n\
S«> ```»\n\
M«```»";
    assert_eq!(render(Language::Markdown, quoted), quoted_expected);

    let diff = "diff --git a/x b/x
index 1..2 100644
--- a/x
+++ b/x
@@ -1,3 +1,3 @@ fn main
 context
--- removed line that looks like a header
+added
\\ No newline at end of file
 tail
+++ after the hunk";
    let diff_expected = "M«diff --git a/x b/x»
M«index 1..2 100644»
M«--- a/x»
M«+++ b/x»
H«@@ -1,3 +1,3 @@»

R«--- removed line that looks like a header»
A«+added»
C«\\ No newline at end of file»

M«+++ after the hunk»";
    assert_eq!(render(Language::Diff, diff), diff_expected);

    let commit = "Add the language table

Body text with a trailer below.
Signed-off-by: A <a@b>
# Please enter the commit message
# ------------------------ >8 ------------------------
diff --git a/x b/x
@@ -1 +1 @@
-old
+new";
    let commit_expected = "H«Add the language table»


K«Signed-off-by:»
C«# Please enter the commit message»
C«# ------------------------ >8 ------------------------»
M«diff --git a/x b/x»
H«@@ -1 +1 @@»
R«-old»
A«+new»";
    assert_eq!(render(Language::GitCommit, commit), commit_expected);

    let todo = "pick 1a2b3c4 First commit
fixup -C 5d6e7f8 Second
exec cargo test
label onto
# comment
pck 9999999 typo";
    let todo_expected = "K«pick» N«1a2b3c4»
K«fixup» K«-C» N«5d6e7f8»
K«exec»
K«label» M«onto»
C«# comment»
";
    assert_eq!(render(Language::GitRebaseTodo, todo), todo_expected);

    assert_eq!(
        render(Language::Plain, "fn \"x\" # y\n`z`"),
        "\n",
        "plain text has no spans"
    );
}

/// A deterministic generator (splitmix64), so the property tests are the same cases on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }
}

/// Fragments that open and close every construct some lexer carries across lines.
const FRAGMENTS: &[&str] = &[
    "\n",
    "\n",
    "\n",
    " ",
    "\t",
    "a",
    "fn",
    "if",
    "1",
    "0x",
    "{",
    "}",
    "(",
    ")",
    "[",
    "]",
    "\"",
    "'",
    "\\",
    "/*",
    "*/",
    "//",
    "#",
    "r#\"",
    "\"#",
    "`",
    "```",
    "~~~",
    "<!--",
    "-->",
    "$",
    "${",
    "@@ -1,2 +1,2 @@",
    "+",
    "-",
    "---",
    "\"\"\"",
    "'''",
    "=",
    ",",
    ".",
    "é",
    "🦀",
    "> ",
    "- ",
    "1. ",
    "pick ",
    "# ------------------------ >8 ------------------------",
    "\r",
    "<<",
    "<<-",
    "EOF",
    "$((",
    "))",
];

fn random_text(rng: &mut Rng, fragments: usize) -> String {
    (0..fragments)
        .map(|_| FRAGMENTS[rng.below(FRAGMENTS.len())])
        .collect()
}

/// Where line `line` of `text` starts.
fn line_offset(text: &str, line: usize) -> usize {
    match line.checked_sub(1) {
        None => 0,
        Some(breaks) => text
            .match_indices('\n')
            .nth(breaks)
            .map_or(text.len(), |(at, _)| at + 1),
    }
}

/// A random char boundary of `text`.
fn boundary(rng: &mut Rng, text: &str) -> usize {
    let boundaries: Vec<usize> = text
        .char_indices()
        .map(|(at, _)| at)
        .chain([text.len()])
        .collect();
    boundaries[rng.below(boundaries.len())]
}

/// Every span of every line is non-empty.
fn assert_spans_well_formed(cache: &LexCache, text: &str) {
    for (index, line) in text.split('\n').enumerate() {
        let mut end = 0;
        for span in cache.spans(index) {
            assert!(span.range.start < span.range.end, "{span:?} in {line:?}");
            assert!(span.range.start >= end, "{span:?} overlaps in {line:?}");
            assert!(span.range.end <= line.len(), "{span:?} past {line:?}");
            assert!(line.is_char_boundary(span.range.start), "{span:?} {line:?}");
            assert!(line.is_char_boundary(span.range.end), "{span:?} {line:?}");
            end = span.range.end;
        }
    }
}

#[test]
fn incremental_lex_equals_full_lex() {
    let mut rng = Rng(0x00A3_ACE5);
    for language in Language::ALL {
        for _ in 0..120 {
            let fragments = rng.below(60);
            let mut text = random_text(&mut rng, fragments);
            let mut cache = LexCache::new(language, &text);
            assert_spans_well_formed(&cache, &text);
            for _ in 0..6 {
                let lines = text.split('\n').count();
                let (first, removed, inserted) = match rng.below(3) {
                    0 => {
                        let at = rng.below(lines);
                        let count = rng.below(3);
                        let block: String = (0..count)
                            .map(|_| {
                                let fragments = rng.below(4);
                                let mut line = random_text(&mut rng, fragments).replace('\n', "");
                                line.push('\n');
                                line
                            })
                            .collect();
                        text.insert_str(line_offset(&text, at), &block);
                        (at, 0, count)
                    }
                    1 if lines > 1 => {
                        let at = rng.below(lines - 1);
                        let count = 1 + rng.below(lines - 1 - at);
                        let start = line_offset(&text, at);
                        let end = line_offset(&text, at + count);
                        text.replace_range(start..end, "");
                        (at, count, 0)
                    }
                    _ => {
                        let a = boundary(&mut rng, &text);
                        let b = boundary(&mut rng, &text);
                        let (a, b) = (a.min(b), a.max(b));
                        let fragments = rng.below(8);
                        let replacement = random_text(&mut rng, fragments);
                        let first = text[..a].matches('\n').count();
                        let removed = text[a..b].matches('\n').count() + 1;
                        let inserted = replacement.matches('\n').count() + 1;
                        text.replace_range(a..b, &replacement);
                        (first, removed, inserted)
                    }
                };
                cache.splice(&text, first, removed, inserted);
                let full = LexCache::new(language, &text);
                assert_eq!(
                    cache, full,
                    "{language:?}: splice({first}, {removed}, {inserted}) of {text:?}"
                );
                assert_eq!(cache.line_count(), text.split('\n').count());
                assert_spans_well_formed(&cache, &text);
            }
        }
    }
}

#[test]
fn splice_stops_where_the_state_settles() {
    let text: String = (0..100).map(|n| format!("let v{n} = {n};\n")).collect();
    let mut cache = LexCache::new(Language::Rust, &text);
    let edited = text.replacen("let v50", "let w50", 1);
    assert_eq!(cache.splice(&edited, 50, 1, 1), 1);
    assert_eq!(cache, LexCache::new(Language::Rust, &edited));
    let opened = format!("/*{edited}");
    assert_eq!(cache.splice(&opened, 0, 1, 1), 101);
    assert_eq!(cache, LexCache::new(Language::Rust, &opened));
    cache.splice(&opened, 1_000, 5, 1);
    assert_eq!(cache, LexCache::new(Language::Rust, &opened));
    assert!(cache.spans(10_000).is_empty());
    assert_eq!(cache.state_at(10_000), cache.state_at(cache.line_count()));
}

#[test]
fn lexing_reads_each_byte_a_bounded_number_of_times() {
    let units = [
        "/*", "*/", "\"\\", "r###\"", "\"##", "'", "'\\", "`", "``", "<!--", "-", "#", "${", "$",
        "@@ -", "\"\"\"", "'''", "[", "{", "](", "<a", "- ", "> ", "é", "=", ",", "<<", "<<-x ",
        "$((", "> ```",
    ];
    let mut rng = Rng(0x11FE);
    let mut lines: Vec<String> = units.iter().map(|unit| unit.repeat(2000)).collect();
    lines.extend((0..40).map(|_| {
        let fragments = (0..3000)
            .map(|_| FRAGMENTS[rng.below(FRAGMENTS.len())])
            .filter(|fragment| !fragment.contains('\n'));
        fragments.collect::<String>()
    }));
    let openers = [
        "",
        "/* a",
        "\"a",
        "r##\"a",
        "'a",
        "\"\"\"a",
        "'''a",
        "```",
        "<!-- a",
        "@@ -1,9 +1,9 @@",
        "x = [",
        "# ------------------------ >8 ------------------------",
        "cat <<EOF",
        "> ```",
    ];
    for language in Language::ALL {
        for opener in openers {
            let (_, state) = language.lex_line(LexState::START, opener);
            for line in &lines {
                BYTE_READS.with(|reads| reads.set(0));
                let (spans, _) = language.lex_line(state, line);
                let reads = BYTE_READS.with(std::cell::Cell::get);
                assert!(
                    reads <= 8 * line.len() + 16,
                    "{language:?} read {reads} bytes for a {}-byte line {:?}…",
                    line.len(),
                    &line[..line.char_indices().nth(16).map_or(line.len(), |(at, _)| at)]
                );
                assert!(spans.len() <= line.len());
            }
        }
    }
}

#[test]
fn lex_state_is_small() {
    assert!(std::mem::size_of::<LexState>() <= 24);
}

/// The prefix [`Language::line_break`] gives the new line.
fn prefix(
    language: Language,
    state: LexState,
    before: &str,
    after: &str,
    style: IndentStyle,
) -> String {
    language.line_break(state, before, after, style).prefix
}

/// Each line's indent as `indent_column` computes it from the lines above.
fn reindent(language: Language, text: &str, style: IndentStyle) -> Vec<(usize, usize)> {
    let cache = LexCache::new(language, text);
    let mut previous: Option<&str> = None;
    let mut found = Vec::new();
    for (index, line) in text.split('\n').enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let want = language.indent_column(cache.state_at(index), previous, line, style);
        found.push((want, style.columns_of(line)));
        previous = Some(line);
    }
    found
}

#[test]
fn indent_rules() {
    let rust = "fn main() {
    let x = foo(
        a,
    );
    let y = outer(inner(
        b,
    ));
    if y {
        z
    } else {
        w
    }
    match v {
        A => {
            1
        }
        B => 2,
    }
    items.iter().for_each(|i| {
        use_it(i);
    });
    let s = \"a string
  keeps its own lines\";
    /* a comment
    keeps the line above's */
}";
    for (index, (want, have)) in reindent(Language::Rust, rust, IndentStyle::DEFAULT)
        .into_iter()
        .enumerate()
    {
        assert_eq!(want, have, "rust line {index}");
    }
    let style = IndentStyle::DEFAULT;
    let start = LexState::START;
    assert_eq!(
        prefix(Language::Rust, start, "fn main() {", "", style),
        "    "
    );
    assert_eq!(prefix(Language::Rust, start, "fn main() {", "}", style), "");
    let (_, inside) = Language::Rust.lex_line(start, "fn main() {");
    assert_eq!(
        prefix(Language::Rust, inside, "    let x = foo(", "", style),
        "        "
    );
    assert_eq!(
        prefix(Language::Rust, inside, "    if y {", "}", style),
        "    "
    );
    assert_eq!(
        prefix(Language::Rust, inside, "    let s = \"abc", "def\";", style),
        ""
    );
    let tabs = IndentStyle {
        width: 4,
        tabs: true,
    };
    assert_eq!(
        prefix(Language::Rust, inside, "    if y {", "", tabs),
        "\t\t"
    );
    assert_eq!(tabs.render(6), "\t  ");
    assert_eq!(tabs.columns_of("\t  x"), 6);
    assert_eq!(style.render(usize::MAX).len(), MAX_INDENT_COLUMNS);

    assert_eq!(
        Language::Rust.line_break(start, "fn main() {  ", "  }", style),
        LineBreak {
            keep: 11,
            skip: 2,
            prefix: String::new()
        }
    );
    assert_eq!(
        Language::Rust.line_break(inside, "    let s = \"a  ", "  b\";", style),
        LineBreak {
            keep: 16,
            skip: 0,
            prefix: String::new()
        }
    );
    let (_, shell_string) = Language::Shell.lex_line(start, "  msg=\"open");
    assert_eq!(
        Language::Shell.line_break(shell_string, "  still ", " more", style),
        LineBreak {
            keep: 8,
            skip: 0,
            prefix: String::new()
        }
    );

    let markdown = |before: &str| prefix(Language::Markdown, start, before, "", style);
    assert_eq!(markdown("- item"), "- ");
    assert_eq!(markdown("  * nested"), "  * ");
    assert_eq!(markdown("1. first"), "2. ");
    assert_eq!(markdown("9) ninth"), "10) ");
    assert_eq!(markdown("- [x] done"), "- [ ] ");
    assert_eq!(markdown("- "), "");
    assert_eq!(markdown("3."), "");
    assert_eq!(markdown("plain text"), "");
    assert_eq!(markdown("    indented"), "    ");
    assert_eq!(markdown("---"), "", "a thematic break is not an item");
    let ended = |before: &str| Language::Markdown.line_break(start, before, "", style);
    assert_eq!(ended("- ").keep, 0);
    assert_eq!(ended("  3. ").keep, 0);
    assert_eq!(ended("- item  ").keep, 6);
    assert_eq!(markdown("> quoted text"), "> ");
    assert_eq!(markdown("> > nested"), "> > ");
    assert_eq!(markdown("  >no space"), "  >");
    assert_eq!(markdown("> - item"), "> - ");
    assert_eq!(markdown("> 1. first"), "> 2. ");
    assert_eq!(
        ended("> - "),
        LineBreak {
            keep: 1,
            skip: 0,
            prefix: "> ".to_owned()
        }
    );
    assert_eq!(
        ended("> "),
        LineBreak {
            keep: 0,
            skip: 0,
            prefix: String::new()
        }
    );
    let (_, quoted_fence) = Language::Markdown.lex_line(start, "> ```");
    assert_eq!(
        prefix(
            Language::Markdown,
            quoted_fence,
            ">     let x = 1;",
            "",
            style
        ),
        ">     "
    );
    let (_, fence) = Language::Markdown.lex_line(start, "```");
    assert_eq!(
        prefix(Language::Markdown, fence, "- in code", "", style),
        ""
    );
    let md_indent =
        |above: &str, line: &str| Language::Markdown.indent_column(start, Some(above), line, style);
    assert_eq!(md_indent("- item text", "continued"), 2);
    assert_eq!(md_indent("10. item", "continued"), 4);
    assert_eq!(md_indent("- [ ] task", "continued"), 6);
    assert_eq!(md_indent("- a", "  - b"), 2);
    assert_eq!(md_indent("plain", "x"), 0);
    assert_eq!(md_indent("   plain", "x"), 3);

    for language in [
        Language::Plain,
        Language::Toml,
        Language::Shell,
        Language::Diff,
        Language::GitCommit,
        Language::GitRebaseTodo,
    ] {
        assert_eq!(
            prefix(language, start, "\t  echo hi {", "", style),
            "\t  ",
            "{language:?}"
        );
        assert_eq!(
            language.indent_column(start, Some("\tfoo {"), "}", style),
            4,
            "{language:?}"
        );
        assert_eq!(language.indent_column(start, None, "  x", style), 0);
    }
}

#[test]
fn fill_prefix_rules() {
    let style = IndentStyle::DEFAULT;
    let cases: &[(Language, &str, usize, &str)] = &[
        (Language::Rust, "    /// doc text", 8, "    /// "),
        (Language::Rust, "    //! inner", 8, "    //! "),
        (Language::Rust, "    // note", 7, "    // "),
        (Language::Rust, "    let x = 1;", 4, "    "),
        (Language::Markdown, "- item text", 2, "  "),
        (Language::Markdown, "  10. item", 6, "      "),
        (Language::Markdown, "- [ ] task", 6, "      "),
        (Language::Markdown, "> > quoted", 4, "> > "),
        (Language::Shell, "  # comment", 4, "  # "),
        (Language::Toml, "## two", 3, "## "),
        (Language::GitCommit, "- a bullet in the body", 2, "  "),
        (Language::GitCommit, "Body text", 0, ""),
        (Language::Plain, "  text", 2, "  "),
        (Language::Diff, "+added", 0, ""),
    ];
    for &(language, line, first, rest) in cases {
        assert_eq!(
            language.fill_prefix(line, style),
            FillPrefix {
                first,
                rest: rest.to_owned()
            },
            "{language:?} {line:?}"
        );
    }
}

/// `count` copies of `unit`.
fn chars(unit: &str, count: usize) -> String {
    unit.repeat(count)
}

#[test]
fn git_commit_diagnostics() {
    let commit = Language::GitCommit;
    let fifty = chars("s", 50);
    let fifty_one = chars("s", 51);
    assert_eq!(commit.diagnostics(&format!("{fifty}\n\nbody\n")), vec![]);
    assert_eq!(
        commit.diagnostics(&format!("{fifty_one}\n")),
        vec![Diag {
            range: 50..51,
            line: 0,
            severity: Severity::Warning,
            kind: DiagKind::SummaryTooLong,
            message: "the summary is 51 characters; keep it to 50".to_owned(),
        }]
    );
    let wide = chars("é", 51);
    let found = commit.diagnostics(&format!("{wide}  \r\n"));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].range, 100..102);
    assert_eq!(
        commit.diagnostics(&format!("{}  \r\n", chars("é", 50))),
        vec![]
    );
    let text = format!("# Please enter\n\n{fifty_one}\n");
    let found = commit.diagnostics(&text);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].line, 2);
    assert_eq!(&text[found[0].range.clone()], "s");

    let second = |text: &str| {
        commit
            .diagnostics(text)
            .into_iter()
            .map(|diag| (diag.kind, diag.line, text[diag.range].to_owned()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        second("Summary\nBody"),
        vec![(DiagKind::SecondLineNotBlank, 1, "Body".to_owned())]
    );
    assert_eq!(
        second("Summary\n# a comment\n  Body  \r\n"),
        vec![(DiagKind::SecondLineNotBlank, 2, "Body".to_owned())]
    );
    assert_eq!(second("Summary\n\nBody"), vec![]);
    assert_eq!(second("Summary\n# a comment\n\nBody"), vec![]);
    assert_eq!(second("Summary\n   \nBody"), vec![]);
    assert_eq!(second("Summary"), vec![]);
    assert_eq!(second(""), vec![]);
    assert_eq!(
        commit
            .diagnostics(&format!("{fifty_one}\nbody"))
            .iter()
            .map(|diag| diag.kind)
            .collect::<Vec<_>>(),
        vec![DiagKind::SummaryTooLong, DiagKind::SecondLineNotBlank]
    );
    let verbose =
        format!("\n# ------------------------ >8 ------------------------\n{fifty_one}\nx\n");
    assert_eq!(commit.diagnostics(&verbose), vec![]);
    assert_eq!(commit.diagnostics_with(&fifty_one, 72), vec![]);
    assert_eq!(commit.diagnostics_with(&fifty, 49).len(), 1);
    assert_eq!(DiagKind::SummaryTooLong.name(), "summary-too-long");

    let todo = "pick 1a2b3c4 x\npck 5d6e7f8 typo\n\n# pick in a comment\n  exec make\r\nbogus\n";
    let found = Language::GitRebaseTodo.diagnostics(todo);
    let flagged: Vec<_> = found
        .iter()
        .map(|diag| {
            (
                diag.line,
                &todo[diag.range.clone()],
                diag.severity,
                diag.kind,
            )
        })
        .collect();
    assert_eq!(
        flagged,
        vec![
            (1, "pck", Severity::Error, DiagKind::UnknownRebaseCommand),
            (5, "bogus", Severity::Error, DiagKind::UnknownRebaseCommand),
        ]
    );
    assert!(found[0].message.contains("`pck`"));

    for language in [
        Language::Plain,
        Language::Markdown,
        Language::Toml,
        Language::Rust,
        Language::Shell,
        Language::Diff,
    ] {
        assert_eq!(language.diagnostics(&fifty_one), vec![], "{language:?}");
    }
}

#[test]
fn commit_editmsg_detects_fills_at_72_and_flags_a_51_character_summary() {
    let summary = chars("x", 51);
    let text = format!("{summary}\n\n# Please enter the commit message for your changes.\n");
    let language = Language::detect("/repo/.git/COMMIT_EDITMSG", &summary);
    assert_eq!(language, Language::GitCommit);
    assert_eq!(language.fill_column(), 72);
    let found = language.diagnostics(&text);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].kind, DiagKind::SummaryTooLong);
    assert_eq!(found[0].range, 50..51);
}

#[test]
fn contracted_scalar_core_matches_the_std_forms() {
    for byte in 0..=u8::MAX {
        assert_eq!(line_space(byte), b" \t\r".contains(&byte), "{byte:#04x}");
        assert_eq!(
            rust::ident_byte(byte),
            byte.is_ascii_alphanumeric() || byte == b'_' || !byte.is_ascii(),
            "{byte:#04x}"
        );
        assert_eq!(
            toml::bare_key_byte(byte),
            byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-',
            "{byte:#04x}"
        );
        assert_eq!(
            toml::value_byte(byte),
            byte.is_ascii_alphanumeric() || b"_+-.:".contains(&byte),
            "{byte:#04x}"
        );
        assert_eq!(
            shell::metachar(byte),
            b";&|()<>".contains(&byte),
            "{byte:#04x}"
        );
    }
    for c in (0..=0x10_FFFF).filter_map(char::from_u32) {
        let mut buffer = [0; 4];
        let encoded = c.encode_utf8(&mut buffer);
        assert_eq!(
            usize::from(rust::utf8_width(encoded.as_bytes()[0])),
            c.len_utf8(),
            "{c:?}"
        );
    }
    for byte in 0x80..=0xBF {
        assert_eq!(rust::utf8_width(byte), 0, "{byte:#04x}");
    }
    for width in [0, 1, 2, 4, 8, usize::MAX] {
        assert_eq!(tab_stop(width), width.max(1));
    }
    for columns in [0, 1, 1023, 1024, 1025, usize::MAX] {
        assert_eq!(indent_columns_cap(columns), columns.min(MAX_INDENT_COLUMNS));
    }
    for value in (0..=30)
        .chain(429_496_700..=429_496_760)
        .chain([u32::MAX - 1, u32::MAX])
    {
        for byte in b'0'..=b'9' {
            let exact = u64::from(value) * 10 + u64::from(byte - b'0');
            let want = if value <= 429_496_728 {
                u32::try_from(exact).expect("below the bound nothing wraps")
            } else {
                u32::MAX
            };
            assert_eq!(decimal_push(value, byte), want, "{value} {byte}");
        }
    }
}
