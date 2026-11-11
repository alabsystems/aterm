// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// Every selector spelling of the grammar, aliases included, and the
/// selector it parses to; then the spellings that are refused.
#[test]
fn selector_spellings_parse() {
    use FunctionKind::{Csi, Dcs, Osc};
    use SelectorParam::{NotQuery, Query, Wildcard};
    let sel = |function, major, suffix: Option<&str>, params: &[SelectorParam]| SequenceSelector {
        function,
        major,
        suffix: suffix.map(str::to_owned),
        params: params.to_vec(),
    };
    for (spelling, want) in [
        ("*", SequenceSelector::catch_all()),
        ("response any", SequenceSelector::catch_all()),
        ("OSC 4", sel(Osc, Some(4), None, &[])),
        ("OSC 4;*;?", sel(Osc, Some(4), None, &[Wildcard, Query])),
        (
            "OSC 4;*;<not-question>",
            sel(Osc, Some(4), None, &[Wildcard, NotQuery]),
        ),
        (
            "OSC 52 set",
            sel(Osc, Some(52), None, &[Wildcard, NotQuery]),
        ),
        ("OSC 52 query", sel(Osc, Some(52), None, &[Wildcard, Query])),
        ("OSC 4 query", sel(Osc, Some(4), None, &[Wildcard, Query])),
        ("OSC 4 set", sel(Osc, Some(4), None, &[Wildcard, NotQuery])),
        ("OSC 21 set named", sel(Osc, Some(21), None, &[Wildcard])),
        ("OSC 21 set indexed", sel(Osc, Some(21), None, &[Wildcard])),
        ("CSI 20 t", sel(Csi, Some(20), Some("t"), &[])),
        ("CSI  20  t", sel(Csi, Some(20), Some("t"), &[])),
        ("CSI t", sel(Csi, None, Some("t"), &[])),
        ("CSI ? h", sel(Csi, Some(0), Some("h"), &[])),
        ("CSI  ?  h", sel(Csi, Some(0), Some("h"), &[])),
        ("OSC  52", sel(Osc, Some(52), None, &[])),
        ("OSC\t4;*;?", sel(Osc, Some(4), None, &[Wildcard, Query])),
        ("DCS 2000p", sel(Dcs, None, Some("2000p"), &[])),
        ("DCS 2000 p", sel(Dcs, None, Some("2000p"), &[])),
        ("DCS  2000  p", sel(Dcs, None, Some("2000p"), &[])),
        ("DCS *", sel(Dcs, None, None, &[])),
        ("DCS", sel(Dcs, None, None, &[])),
    ] {
        assert_eq!(SequenceSelector::parse(spelling), Ok(want), "{spelling:?}");
    }
    assert!(matches!(
        SequenceSelector::parse("XYZ 1"),
        Err(SelectorParseError::UnknownFunction(_))
    ));
    assert!(matches!(
        SequenceSelector::parse("OSC +4"),
        Err(SelectorParseError::BadMajor(_))
    ));
    assert!(matches!(
        SequenceSelector::parse("CSI +20 t"),
        Err(SelectorParseError::BadMajor(_))
    ));
    assert_eq!(SequenceSelector::parse(""), Err(SelectorParseError::Empty));
}

/// `(selector, sequence, matches?)` — each selector against the
/// sequences its original one-case test drove, including the near misses.
#[test]
fn selector_matching() {
    let osc = |major: u32, params: &[&str]| {
        DispatchedSequence::osc(major, params.iter().map(|&p| p.to_owned()))
    };
    let csi = |major: u32, final_byte: char| DispatchedSequence::csi(Some(major), final_byte, []);
    for (selector, sequence, matches) in [
        ("*", osc(4, &[]), true),
        ("*", csi(20, 't'), true),
        ("*", DispatchedSequence::dcs("2000p"), true),
        ("OSC 52 set", osc(52, &["c", "SGVsbG8="]), true),
        ("OSC 52 set", osc(52, &["c", "?"]), false),
        ("OSC 52 query", osc(52, &["c", "?"]), true),
        ("OSC 4", osc(4, &["3", "?"]), true),
        ("OSC 4", osc(4, &["5", "red"]), true),
        ("OSC 4", osc(5, &[]), false),
        ("CSI t", csi(20, 't'), true),
        ("CSI t", csi(1, 't'), true),
        ("CSI t", csi(1, 'h'), false),
        ("CSI 20 t", csi(20, 't'), true),
        ("CSI 20 t", csi(21, 't'), false),
        ("CSI ? h", csi(0, 'h'), true),
        ("CSI ? h", csi(25, 'h'), false),
        ("DCS 2000p", DispatchedSequence::dcs("2000p"), true),
        ("DCS 2000p", DispatchedSequence::dcs("1000p"), false),
        ("DCS *", DispatchedSequence::dcs("2000p"), true),
        ("DCS *", DispatchedSequence::dcs("1000p"), true),
        ("OSC 4;*;?", osc(4, &["3"]), false),
        ("OSC 4;*", osc(4, &["3", "anything"]), true),
    ] {
        let sel = SequenceSelector::parse(selector).expect("parses");
        assert_eq!(
            sel.matches(&sequence),
            matches,
            "{selector} vs {sequence:?}"
        );
    }
}

#[test]
fn not_query_param_rejects_empty_and_question() {
    assert!(!SelectorParam::NotQuery.matches(""));
    assert!(!SelectorParam::NotQuery.matches("?"));
    assert!(SelectorParam::NotQuery.matches("red"));
}
