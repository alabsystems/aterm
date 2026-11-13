// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn the_code_view_blanks_every_literal_and_comment_in_place() {
    let src = "let a = \"x // y\"; // z\nlet b = r#\"multi\nline\"#; /* c\nd */ e\n\
               'x' '\\'' 'a b\n#[cfg(all(test, feature = \"x\"))] // why\nlet c = br\"}\";";
    let lexed = blank_non_code(src);
    assert_eq!(
        lexed.lines().collect::<Vec<_>>(),
        [
            "let a = \"      \";",
            "let b =",
            "      ;",
            "     e",
            "         'a b",
            "#[cfg(all(test, feature = \" \"))]",
            "let c =   \" \";",
        ]
    );
    assert_eq!(lexed.lines().count(), src.lines().count());
    assert_eq!(blank_non_code("no newline").lines().count(), 1);
    assert_eq!(
        blank_non_code("a\n\n").lines().count(),
        "a\n\n".lines().count()
    );
}

#[test]
fn the_text_view_keeps_literals_and_drops_comments_byte_aligned() {
    let src = "f(\"/dev/x\"); // see \"/dev/y\"\n/* é */ g('é', b'\\n', c\"Ab\")\n";
    let lexed = lex(src);
    assert_eq!(lexed.code.len(), src.len());
    assert_eq!(lexed.text.len(), src.len());
    let text: Vec<&str> = lexed.text.lines().collect();
    assert_eq!(text[0].trim_end(), "f(\"/dev/x\");");
    assert_eq!(text[1].trim_start(), "g('é', b'\\n', c\"Ab\")");
    let code: Vec<&str> = lexed.code.lines().collect();
    assert_eq!(code[0].trim_end(), "f(\"      \");");
    let at = code[1]
        .find('"')
        .expect("a one-line string keeps its quotes");
    assert_eq!(&text[1][at - 1..at + 4], "c\"Ab\"");
}

#[test]
fn lifetimes_labels_and_raw_identifiers_are_code() {
    let src = "fn f<'a>(x: &'a str) { 'outer: loop { r#type(x); } }";
    assert_eq!(lex(src).code, src);
    assert!(spans(src).is_empty());
}
