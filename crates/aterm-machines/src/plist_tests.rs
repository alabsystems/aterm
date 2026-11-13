// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const VM_CURRENT_HOST_GLOBAL: &[u8] =
    include_bytes!("../fixtures/prefs/vm-27.0-currenthost-global.plist");

const VM_EVERY_TYPE: &[u8] = include_bytes!("../fixtures/prefs/vm-27.0-every-type.plist");

const VM_MISSING_DOMAIN: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict/>\n</plist>\n";

fn get<'a>(dict: &'a PValue, key: &str) -> &'a PValue {
    let PValue::Dict(entries) = dict else {
        panic!("not a dict: {dict}");
    };
    &entries
        .iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("no key {key}"))
        .1
}

#[test]
fn the_vm_current_host_global_domain_parses_with_its_caps_lock_mapping() {
    let root = parse(VM_CURRENT_HOST_GLOBAL).unwrap();
    let mapping = get(&root, "com.apple.keyboard.modifiermapping.1452-1-0");
    assert_eq!(
        mapping,
        &PValue::Array(vec![PValue::Dict(vec![
            (
                "HIDKeyboardModifierMappingDst".into(),
                PValue::Int(30_064_771_296)
            ),
            (
                "HIDKeyboardModifierMappingSrc".into(),
                PValue::Int(30_064_771_129)
            ),
        ])])
    );
    assert_eq!(
        get(&root, "com.apple.trackpad.enableSecondaryClick"),
        &PValue::Bool(true)
    );
    assert_eq!(
        get(&root, "com.apple.trackpad.threeFingerDragGesture"),
        &PValue::Bool(false)
    );
    assert_eq!(get(&root, "com.apple.trackpad.version"), &PValue::Int(5));
    assert_eq!(
        get(&root, "com.apple.gms.enhancedSiri.lastUpdated"),
        &PValue::Date("2026-10-16T22:40:34Z".into())
    );
    let PValue::Data(bytes) = get(&root, "com.apple.gms.availability.unifiedReasons") else {
        panic!("not data");
    };
    assert_eq!(
        String::from_utf8_lossy(bytes),
        r#"{"com.apple.Settings.AppleIntelligence.Partner.ChatGPT":[{"unavailable":{"_0":{"partnerNotSelected":{}}}}]}"#
    );
}

#[test]
fn a_document_is_written_back_exactly_as_defaults_export_printed_it() {
    for fixture in [
        VM_CURRENT_HOST_GLOBAL,
        VM_EVERY_TYPE,
        VM_MISSING_DOMAIN.as_bytes(),
    ] {
        let value = parse(fixture).unwrap();
        assert_eq!(document(&value), String::from_utf8_lossy(fixture));
    }
}

#[test]
fn a_missing_domain_is_an_empty_dictionary() {
    assert_eq!(
        parse(VM_MISSING_DOMAIN.as_bytes()).unwrap(),
        PValue::Dict(Vec::new())
    );
}

#[test]
fn every_type_the_vm_wrote_reads_back_with_its_type() {
    let root = parse(VM_EVERY_TYPE).unwrap();
    assert_eq!(get(&root, "int"), &PValue::Int(5));
    assert_eq!(get(&root, "neg"), &PValue::Int(-30_064_771_129));
    assert_eq!(get(&root, "real"), &PValue::Real(1.5));
    assert_eq!(get(&root, "t"), &PValue::Bool(true));
    assert_eq!(get(&root, "f"), &PValue::Bool(false));
    assert_eq!(get(&root, "data"), &PValue::Data(vec![0, 1, 2, 0xff]));
    assert_eq!(
        get(&root, "date"),
        &PValue::Date("2026-10-16T22:40:34Z".into())
    );
    assert_eq!(get(&root, "emptystr"), &PValue::Str(String::new()));
    assert_eq!(get(&root, "emptyarr"), &PValue::Array(Vec::new()));
    assert_eq!(get(&root, "emptydict"), &PValue::Dict(Vec::new()));
    assert_eq!(
        get(&root, "str"),
        &PValue::Str("a & b <c> \"q\" 's' \u{e9} \u{263a}".into())
    );
    assert_eq!(
        get(&root, "dict"),
        &PValue::Dict(vec![
            (
                "a".into(),
                PValue::Dict(vec![("n".into(), PValue::Array(vec![PValue::Real(0.25)]))])
            ),
            ("b".into(), PValue::Int(2)),
        ])
    );
}

fn every_type() -> PValue {
    PValue::Dict(vec![
        ("b".into(), PValue::Bool(true)),
        ("i".into(), PValue::Int(i64::MIN)),
        ("r".into(), PValue::Real(0.1)),
        ("tiny".into(), PValue::Real(1e-7)),
        ("big".into(), PValue::Real(6.02e23)),
        ("s".into(), PValue::Str("<a> & 'b' \"c\" é\ttab".into())),
        ("empty".into(), PValue::Str(String::new())),
        ("d".into(), PValue::Data((0..=255).collect())),
        ("nodata".into(), PValue::Data(Vec::new())),
        ("t".into(), PValue::Date("2026-10-16T22:40:34Z".into())),
        (
            "a".into(),
            PValue::Array(vec![
                PValue::Array(Vec::new()),
                PValue::Dict(Vec::new()),
                PValue::Dict(vec![("deep".into(), PValue::Data(vec![7; 100]))]),
            ]),
        ),
    ])
}

#[test]
fn a_fragment_and_a_document_read_back_to_the_value_they_were_made_from() {
    let value = every_type();
    assert_eq!(parse(fragment(&value).as_bytes()).unwrap(), value);
    assert_eq!(parse(document(&value).as_bytes()).unwrap(), value);
    assert!(!fragment(&value).contains('\n'));
}

#[test]
fn a_fragment_is_the_one_line_form_the_vm_accepted() {
    let mapping = PValue::Array(vec![PValue::Dict(vec![
        (
            "HIDKeyboardModifierMappingSrc".into(),
            PValue::Int(30_064_771_129),
        ),
        (
            "HIDKeyboardModifierMappingDst".into(),
            PValue::Int(30_064_771_296),
        ),
    ])]);
    assert_eq!(
        fragment(&mapping),
        "<array><dict><key>HIDKeyboardModifierMappingSrc</key><integer>30064771129</integer><key>HIDKeyboardModifierMappingDst</key><integer>30064771296</integer></dict></array>"
    );
    assert_eq!(
        fragment(&PValue::Data(vec![0, 1, 2, 0xff])),
        "<data>AAEC/w==</data>"
    );
}

#[test]
fn reals_are_written_as_corefoundation_writes_them() {
    assert_eq!(real_text(0.1), "0.10000000000000001");
    assert_eq!(real_text(1e-7), "9.9999999999999995e-08");
    assert_eq!(real_text(30_064_771_129.5), "30064771129.5");
    assert_eq!(real_text(3.0), "3");
    assert_eq!(real_text(0.25), "0.25");
    assert_eq!(real_text(f64::from(0.1_f32)), "0.10000000149011612");
    assert_eq!(real_text(f64::INFINITY), "+infinity");
    assert_eq!(real_text(f64::NEG_INFINITY), "-infinity");
    assert_eq!(real_text(f64::NAN), "nan");
    assert_eq!(real_text(1e300), "1.0000000000000001e+300");
    for r in [0.1, 1e-7, 1e300, -2.5, 0.083_333_333, f64::MIN_POSITIVE] {
        assert_eq!(parse_real(&real_text(r)), Some(r));
    }
}

#[test]
fn integers_read_in_decimal_or_hex_and_refuse_what_does_not_fit() {
    assert_eq!(parse_integer("30064771129"), Some(30_064_771_129));
    assert_eq!(parse_integer("-1"), Some(-1));
    assert_eq!(parse_integer("+7"), Some(7));
    assert_eq!(parse_integer("0x7000000E0"), Some(30_064_771_296));
    assert_eq!(parse_integer("18446744073709551615"), None);
    assert_eq!(parse_integer("1.5"), None);
    assert_eq!(parse_integer(""), None);
    assert_eq!(parse_integer("0x"), None);
}

#[test]
fn a_broken_document_is_refused_with_where_it_broke() {
    for (text, needle) in [
        (
            "<plist><dict><key>a</key></dict></plist>",
            "where a value should be",
        ),
        (
            "<plist><dict><key>a</key><integer>x</integer></dict></plist>",
            "<integer>x",
        ),
        ("<plist><array><string>a</array></plist>", "inside <string>"),
        ("<plist><dict>", "ends where a tag"),
        (
            "<plist><string>a &bogus; b</string></plist>",
            "unknown entity",
        ),
        ("<plist><string>a & b</string></plist>", "bare '&'"),
        ("<plist><data>!!!!</data></plist>", "<data>"),
        ("<plist><integer/></plist>", "not a value"),
        ("<plist><true/></plist>trailing", "after the root"),
        ("<plist><blob/></plist>", "not a value"),
        ("<plist/>", "empty <plist/>"),
    ] {
        let err = parse(text.as_bytes()).unwrap_err();
        assert!(err.contains(needle), "{text}: {err}");
        assert!(err.starts_with("plist: at byte "), "{err}");
    }
}

#[test]
fn cdata_comments_and_a_repeated_key_read_as_corefoundation_reads_them() {
    let text = "<!-- c --><plist version=\"1.0\"><dict><!-- x --><key>a</key><string><![CDATA[<raw> & ]]>tail</string><key>a</key><integer>2</integer></dict></plist>";
    assert_eq!(
        parse(text.as_bytes()).unwrap(),
        PValue::Dict(vec![("a".into(), PValue::Int(2))])
    );
    let text = "<string><![CDATA[<raw> & ]]>tail</string>";
    assert_eq!(
        parse(text.as_bytes()).unwrap(),
        PValue::Str("<raw> & tail".into())
    );
}

#[test]
fn an_integer_past_the_signed_range_fails_only_its_own_key_of_a_domain() {
    let text = "<plist version=\"1.0\"><dict><key>apps</key><array><dict><key>flags</key><integer>9223372036854775815</integer></dict></array><key>k</key><true/><key>big</key><integer>13713454596331600536</integer></dict></plist>";
    let entries = parse_domain(text.as_bytes()).unwrap();
    let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["apps", "k", "big"]);
    assert_eq!(entries[1].1, Ok(PValue::Bool(true)));
    for (key, value) in [&entries[0], &entries[2]] {
        let why = value.clone().unwrap_err();
        assert!(why.contains("past the 64-bit signed range"), "{key}: {why}");
    }
    assert!(
        parse(text.as_bytes())
            .unwrap_err()
            .contains("<integer>9223372036854775815")
    );
    let broken = "<plist><dict><key>k</key><integer>12x</integer></dict></plist>";
    assert!(
        parse_domain(broken.as_bytes())
            .unwrap_err()
            .contains("<integer>12x")
    );
}

#[test]
fn a_domain_whose_root_is_not_a_dictionary_is_refused() {
    assert_eq!(
        parse_domain(VM_MISSING_DOMAIN.as_bytes()).unwrap(),
        Vec::new()
    );
    let err = parse_domain(b"<plist><array/></plist>").unwrap_err();
    assert!(err.contains("not a dictionary"), "{err}");
    let entries = parse_domain(VM_EVERY_TYPE).unwrap();
    let PValue::Dict(whole) = parse(VM_EVERY_TYPE).unwrap() else {
        panic!("not a dict");
    };
    let held: Vec<(String, PValue)> = entries.into_iter().map(|(k, v)| (k, v.unwrap())).collect();
    assert_eq!(held, whole);
}

#[test]
fn nesting_past_the_guard_is_refused_rather_than_overflowing_the_stack() {
    let deep = format!("{}{}", "<array>".repeat(10_000), "</array>".repeat(10_000));
    let err = parse(deep.as_bytes()).unwrap_err();
    assert!(err.contains("nest too deeply"), "{err}");
}
