// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn text(flags: FlagWords, emit: impl FnOnce(&mut dyn FieldSink)) -> String {
    let mut sink = TextSink::new("OK", flags);
    emit(&mut sink);
    sink.finish()
}

fn json(emit: impl FnOnce(&mut dyn FieldSink)) -> String {
    let mut sink = JsonSink::new();
    emit(&mut sink);
    sink.finish()
}

type Case<'a> = (&'a dyn Fn(&mut dyn FieldSink), &'a str, &'a str);

#[test]
fn each_kind_spells_its_documented_bytes() {
    let rec = [("arms", Cell::Count(3)), ("past", Cell::Count(1))];
    let cases: [Case<'_>; 12] = [
        (&|s| s.count("frames", 42), " frames=42", "\"frames\":42"),
        (
            &|s| s.decimal("p99_ms", Decimal::new(1.005_1, 2)),
            " p99_ms=1.01",
            "\"p99_ms\":1.01",
        ),
        (
            &|s| s.decimal("threshold_ms", Decimal::new(33.333_333, 1)),
            " threshold_ms=33.3",
            "\"threshold_ms\":33.3",
        ),
        (
            &|s| s.decimal("lead_ms", Decimal::new(-0.004, 2)),
            " lead_ms=-0.00",
            "\"lead_ms\":-0.00",
        ),
        (&|s| s.flag("valid", true), " valid=1", "\"valid\":true"),
        (&|s| s.flag("valid", false), " valid=0", "\"valid\":false"),
        (
            &|s| s.label("backend", "gpu"),
            " backend=gpu",
            "\"backend\":\"gpu\"",
        ),
        (&|s| s.absent("rows", "busy"), " rows=busy", "\"rows\":null"),
        (
            &|s| s.count_pairs("fires", &[("frame_cap", 2), ("blink", 7)], "none"),
            " fires=frame_cap:2,blink:7",
            "\"fires\":{\"frame_cap\":2,\"blink\":7}",
        ),
        (
            &|s| s.count_pairs("fires", &[], "none"),
            " fires=none",
            "\"fires\":{}",
        ),
        (
            &|s| s.pairs("arms", &[("cursor", Cell::Rec(&rec))], "none"),
            " arms=cursor:3/1",
            "\"arms\":{\"cursor\":{\"arms\":3,\"past\":1}}",
        ),
        (
            &|s| {
                s.opt_count("cols", None, "busy");
                s.opt_decimal("birth_ms", Some(Decimal::new(2.5, 2)), "none");
            },
            " cols=busy birth_ms=2.50",
            "\"cols\":null,\"birth_ms\":2.50",
        ),
    ];
    for (emit, want_text, want_json) in cases {
        assert_eq!(text(FlagWords::ZeroOne, emit), format!("OK{want_text}"));
        assert_eq!(json(emit), format!("{{{want_json}}}"));
    }
}

#[test]
fn fields_keep_their_call_order() {
    let emit = |s: &mut dyn FieldSink| {
        s.count("b", 2);
        s.count("a", 1);
        s.flag("c", true);
    };
    assert_eq!(text(FlagWords::ZeroOne, emit), "OK b=2 a=1 c=1");
    assert_eq!(json(emit), "{\"b\":2,\"a\":1,\"c\":true}");
    assert_eq!(TextSink::new("", FlagWords::ZeroOne).finish(), "");
    assert_eq!(JsonSink::new().finish(), "{}");
}

#[test]
fn the_text_flag_words_are_the_records_choice() {
    for (flags, yes, no) in [
        (FlagWords::ZeroOne, "1", "0"),
        (FlagWords::TrueFalse, "true", "false"),
        (FlagWords::YesNo, "yes", "no"),
    ] {
        let emit = |s: &mut dyn FieldSink| {
            s.flag("on", true);
            s.flag("off", false);
        };
        assert_eq!(text(flags, emit), format!("OK on={yes} off={no}"));
        assert_eq!(json(emit), "{\"on\":true,\"off\":false}");
    }
}

#[test]
fn labels_and_pair_keys_are_escaped_per_syntax() {
    let emit = |s: &mut dyn FieldSink| {
        s.label("plain", "frame_cap");
        s.label("quoted", "a\"b\\c");
        s.label("spaced", "two words\u{1b}");
        s.label("listed", "a:b,c");
        s.count_pairs("ledger", &[("a b", 1), ("a:b", 2), ("c,d", 3)], "none");
    };
    let line = text(FlagWords::ZeroOne, emit);
    assert_eq!(
        line,
        "OK plain=frame_cap quoted=a\"b\\c spaced=two%20words%1B listed=a:b,c \
         ledger=a%20b:1,a%3Ab:2,c%2Cd:3"
    );
    assert_eq!(
        json(emit),
        "{\"plain\":\"frame_cap\",\"quoted\":\"a\\\"b\\\\c\",\
         \"spaced\":\"two words\\u001b\",\"listed\":\"a:b,c\",\
         \"ledger\":{\"a b\":1,\"a:b\":2,\"c,d\":3}}"
    );
    let ledger = line.rsplit_once("ledger=").map_or("", |(_, v)| v);
    let keys: Vec<String> = ledger
        .split(',')
        .filter_map(|pair| pair.split_once(':'))
        .map(|(k, _)| crate::pct::decode(k))
        .collect();
    assert_eq!(keys, ["a b", "a:b", "c,d"], "{ledger}");
}

#[test]
fn a_non_finite_decimal_is_null_in_json() {
    let emit = |s: &mut dyn FieldSink| {
        s.decimal("nan", Decimal::new(f64::NAN, 2));
        s.decimal("ok", Decimal::new(0.5, 2));
    };
    assert_eq!(text(FlagWords::ZeroOne, emit), "OK nan=NaN ok=0.50");
    assert_eq!(json(emit), "{\"nan\":null,\"ok\":0.50}");
}
