// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::field_preedit_display;

#[test]
fn field_preedit_display_rows() {
    enum Span {
        Is(std::ops::Range<usize>),
        Empty,
        Unchecked,
    }
    let rows = [
        (
            "splices at the caret",
            ("find", 2, "中文", Some("中".len())),
            ("fi中文nd", 2 + "中".len(), Span::Is(2.."fi中文".len())),
        ),
        (
            "no platform caret",
            ("ab", 2, "に", None),
            ("abに", "abに".len(), Span::Unchecked),
        ),
        (
            "interior byte offsets floor to char boundaries",
            ("中", 1, "日", Some(2)),
            ("日中", 0, Span::Is(0.."日".len())),
        ),
        (
            "an empty composition is the identity",
            ("query", 3, "", None),
            ("query", 3, Span::Empty),
        ),
    ];
    for (label, (text, caret, preedit, preedit_caret), (want_text, want_caret, want_span)) in rows {
        let (got_text, got_caret, got_span) =
            field_preedit_display(text, caret, preedit, preedit_caret);
        assert_eq!(got_text, want_text, "{label}");
        assert_eq!(got_caret, want_caret, "{label}");
        match want_span {
            Span::Is(span) => assert_eq!(got_span, span, "{label}"),
            Span::Empty => assert!(got_span.is_empty(), "{label}"),
            Span::Unchecked => {}
        }
    }
}
