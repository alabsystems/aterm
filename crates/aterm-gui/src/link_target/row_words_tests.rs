// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::caption_test_support::instructions;
use super::*;

fn previous(row: &[RenderCell]) -> String {
    let words: String = row.iter().map(|cell| cell.ch).collect();
    words.trim().to_owned()
}

fn cells(text: &str) -> Vec<RenderCell> {
    let mut row = blank_row(text.chars().count(), [0; 3], [0; 3], false);
    for (cell, ch) in row.iter_mut().zip(text.chars()) {
        cell.ch = ch;
    }
    row
}

#[test]
fn client_words_match_unicode_string_trimming_without_changing_the_painted_row() {
    let whitespace: String = (0..=0x10ffff)
        .filter_map(char::from_u32)
        .filter(|ch| ch.is_whitespace())
        .collect();
    for body in [
        "",
        " ",
        "site.example",
        "…%AB.example…",
        "猫🦀",
        "\0",
        "\u{200b}",
        "a \t猫\n b",
    ] {
        for left in whitespace.chars().map(Some).chain([None]) {
            for right in whitespace.chars().map(Some).chain([None]) {
                let text: String = left.into_iter().chain(body.chars()).chain(right).collect();
                let row = cells(&text);
                let before = row.clone();
                assert_eq!(row_words(&row), previous(&row), "text={text:?}");
                assert_eq!(row, before);
            }
        }
    }
}

#[test]
#[ignore = "manual paired client-word and row-plus-word allocation/instruction measurement"]
fn client_words_cost() {
    use std::hint::black_box;
    type Words = fn(&[RenderCell]) -> String;
    type Build = fn(&str, usize, Theme, bool, bool) -> Option<Vec<RenderCell>>;
    let long_host = format!("https://{}.evil.example/x", "猫".repeat(40));
    let long_path = format!("https://evil.example/{}", "猫".repeat(100));
    for (name, target, cols, build) in [
        (
            "short-url",
            "https://evil.example/x",
            80,
            caption_row as Build,
        ),
        ("unicode-host", long_host.as_str(), 80, caption_row as Build),
        ("unicode-path", long_path.as_str(), 80, caption_row as Build),
        (
            "literal",
            "https://[2001:db8::ffff:192.0.2.1]:8443/x",
            50,
            caption_row as Build,
        ),
        (
            "blocked",
            "https://evil.example/a b",
            80,
            caption_row as Build,
        ),
        ("narrow", "https://evil.example/x", 12, caption_row as Build),
        ("short-file", "src/lib.rs:42", 80, path_caption_row as Build),
        (
            "unicode-file",
            "src/猫🦀.rs:42",
            80,
            path_caption_row as Build,
        ),
        ("no-row", "https://evil.example/x", 1, caption_row as Build),
    ] {
        let row = build(target, cols, Theme::default(), true, false);
        if let Some(row) = &row {
            assert_eq!(row_words(row), previous(row));
        }
        let mut variants: [(&str, Words); 2] = [("reference", previous), ("candidate", row_words)];
        for sample in 0..5 {
            if sample != 0 {
                variants.swap(0, 1);
            }
            for (variant, words) in variants {
                for scope in ["words", "row-and-words"] {
                    let call = || {
                        if scope == "words" {
                            black_box(row.as_ref().map(|row| words(black_box(row))));
                        } else {
                            black_box(
                                build(
                                    black_box(target),
                                    black_box(cols),
                                    Theme::default(),
                                    true,
                                    false,
                                )
                                .map(|row| {
                                    let text = words(&row);
                                    (row, text)
                                }),
                            );
                        }
                    };
                    for _ in 0..100 {
                        call();
                    }
                    let (_, allocations) = aterm_heap_count::allocations_in(call);
                    let before = instructions();
                    for _ in 0..2000 {
                        call();
                    }
                    let work = before.zip(instructions()).map(|(a, b)| b.saturating_sub(a));
                    println!(
                        "client-words case={name} scope={scope} variant={variant} sample={sample} iterations=2000 allocations={allocations} instructions={work:?}"
                    );
                }
            }
        }
    }
}
