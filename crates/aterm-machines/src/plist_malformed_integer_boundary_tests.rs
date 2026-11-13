// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{parse, parse_domain};
use crate::model::PValue;

#[test]
fn malformed_signed_hex_integers_are_refused_without_panicking() {
    for (text, wanted) in [
        ("0", 0),
        ("+42", 42),
        ("-128", -128),
        ("0x7f", 127),
        ("-0x80", -128),
        ("9223372036854775807", i64::MAX),
        ("-9223372036854775808", i64::MIN),
        ("-0x8000000000000000", i64::MIN),
    ] {
        let document = format!("<integer>{text}</integer>");
        assert_eq!(parse(document.as_bytes()).unwrap(), PValue::Int(wanted));
    }
    let partial = parse_domain(b"<dict><key>large</key><integer>9223372036854775808</integer><key>kept</key><integer>7</integer></dict>").unwrap();
    assert!(
        partial[0].1.is_err(),
        "a valid out-of-range integer remains an unheld domain value"
    );
    assert_eq!(partial[1].1, Ok(PValue::Int(7)));
    let refused: Vec<bool> = [
        "-0x-80000000000000000000000000000000",
        "0x-1",
        "-0x+1",
        "0x+1",
        "--1",
        "+",
    ]
    .into_iter()
    .map(|text| {
        let document = format!("<integer>{text}</integer>");
        std::panic::catch_unwind(|| parse(document.as_bytes())).is_ok_and(|result| result.is_err())
    })
    .collect();
    assert_eq!(
        refused, [true; 6],
        "every malformed form is observed before the aggregate assertion"
    );
}

/// DOCTYPE PROLOG SCANNER REFUSES INTERNAL SUBSETS AND MID-DOCUMENT DECLARATIONS: an internal
/// DTD subset (`[<!ENTITY ...>]`), duplicate `<!DOCTYPE`, or `<!DOCTYPE` inside `<plist>` /
/// `<dict>` cannot terminate early on a quoted `>` or smuggle a root `<dict>`.
/// NEGATIVE CONTROL: a standard `<!DOCTYPE plist PUBLIC ... "...">` (including `>` inside
/// quotes or comments) in the prolog parses cleanly.
#[test]
fn doctype_quotes_comments_and_internal_subsets_do_not_desync_the_prolog() {
    let valid = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0 > test//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd" <!-- > -->>
<plist version="1.0"><dict><key>ok</key><true/></dict></plist>"#;
    assert_eq!(
        parse(valid).expect("quoted and commented '>' in DOCTYPE must parse"),
        PValue::Dict(vec![("ok".into(), PValue::Bool(true))])
    );

    let smuggled_subset = br#"<!DOCTYPE plist [
  <!ENTITY inject "><dict><key>injected</key><true/></dict><!--">
]>
<plist version="1.0"><dict><key>injected</key><false/></dict></plist>"#;
    let subset_err = parse(smuggled_subset).expect_err("internal DTD subset must be refused");
    assert!(subset_err.contains("internal subset"), "{subset_err}");

    for bad in [
        &b"<!DOCTYPE plist><!DOCTYPE plist><plist><dict/></plist>"[..],
        &b"<plist><!DOCTYPE plist><dict/></plist>"[..],
        &b"<plist><dict><!DOCTYPE plist></dict></plist>"[..],
        &b"<!DOCTYPEplist><plist><dict/></plist>"[..],
    ] {
        assert!(
            parse(bad).is_err(),
            "must refuse invalid DOCTYPE placement: {:?}",
            String::from_utf8_lossy(bad)
        );
    }
}
