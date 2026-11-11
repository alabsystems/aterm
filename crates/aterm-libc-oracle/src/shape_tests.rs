// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::rustdoc::fixture::{doc, function};

fn shape_of(text: &str) -> BTreeSet<ShapeRecord> {
    let parsed = Doc::parse(text).expect("parses");
    let public = parsed.public().expect("public");
    public_shape(&parsed, &public, None).expect("shape")
}

fn struct_doc(method_unsafe: bool, with_clone: bool) -> String {
    let impls = if with_clone { "4,5,6,7" } else { "5,6,7" };
    let index = [
        format!(
            r#""2":{{"id":2,"name":"timespec","visibility":"public","inner":{{"struct":{{"kind":{{"plain":{{"fields":[3]}}}},"impls":[{impls}]}}}}}}"#
        ),
        r#""3":{"id":3,"name":"tv_sec","inner":{"struct_field":{"primitive":"i64"}}}"#.into(),
        r#""4":{"id":4,"inner":{"impl":{"trait":{"path":"Clone","id":90,"args":null},"items":[],"is_synthetic":false,"blanket_impl":null,"is_negative":false}}}"#.into(),
        r#""5":{"id":5,"inner":{"impl":{"trait":{"path":"Send","id":91,"args":null},"items":[],"is_synthetic":true,"blanket_impl":null,"is_negative":false}}}"#.into(),
        r#""6":{"id":6,"inner":{"impl":{"trait":{"path":"Debug","id":92,"args":null},"items":[],"is_synthetic":false,"blanket_impl":{"generic":"T"},"is_negative":false}}}"#.into(),
        r#""7":{"id":7,"inner":{"impl":{"trait":null,"items":[8],"is_synthetic":false,"blanket_impl":null}}}"#.into(),
        function(8, "si_pid", true, method_unsafe, ""),
    ]
    .join(",");
    let paths = r#""90":{"crate_id":1,"path":["core","clone","Clone"],"kind":"trait"},
                       "2":{"crate_id":0,"path":["libc","timespec"],"kind":"struct"}"#;
    doc(&[2], &index, paths)
}

#[test]
fn a_struct_shows_its_fields_inherent_items_and_declared_traits_only() {
    let records: Vec<String> = shape_of(&struct_doc(true, true))
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        records,
        [
            "timespec: public type item is struct",
            "timespec.tv_sec: public field",
            "timespec::si_pid: public inherent function",
            "timespec::si_pid: inherent function is a Rust body",
            r#"timespec::si_pid: inherent function header is {"abi":{"C":{"unwind":false}},"is_async":false,"is_const":false,"is_unsafe":true}"#,
            "timespec: explicit trait impl core::clone::Clone",
        ]
    );
}

#[test]
fn a_dropped_trait_and_a_widened_method_are_diffs() {
    let reference = shape_of(&struct_doc(true, true));
    let shim = shape_of(&struct_doc(false, false));
    let diffs = diff(&reference, &shim);
    let clone = ShapeRecord::Trait {
        owner: "timespec".into(),
        kind: "struct".into(),
        path: "core::clone::Clone".into(),
        args: None,
        negative: false,
    };
    assert!(diffs.contains(&ShapeDiff {
        side: Side::Missing,
        record: clone
    }));
    let safety: Vec<(Side, bool)> = diffs
        .iter()
        .filter_map(|d| match &d.record {
            ShapeRecord::InherentHeader { name, header, .. } if name == "si_pid" => {
                Some((d.side, header.contains(r#""is_unsafe":true"#)))
            }
            _ => None,
        })
        .collect();
    assert_eq!(safety, [(Side::Missing, true), (Side::Unexpected, false)]);
    assert_eq!(diffs.len(), 3, "{diffs:#?}");
}

#[test]
fn an_extern_declaration_and_an_inline_body_differ() {
    let declared = shape_of(&doc(&[1], &function(1, "strlen", false, true, ""), ""));
    let inline = shape_of(&doc(&[1], &function(1, "strlen", true, true, ""), ""));
    let diffs: Vec<String> = diff(&declared, &inline)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        diffs,
        [
            "missing strlen: function is an extern declaration",
            "unexpected strlen: function is a Rust body",
        ]
    );
}

#[test]
fn a_crate_local_trait_argument_reads_as_dollar_crate_on_both_sides() {
    let with_arg = |local_id: u64, krate: &str| {
        let index = format!(
            r#""2":{{"id":2,"name":"timespec","inner":{{"struct":{{"kind":{{"plain":{{"fields":[]}}}},"impls":[4]}}}}}},
                   "4":{{"id":4,"inner":{{"impl":{{"trait":{{"path":"PartialEq","id":93,"args":{{"angle_bracketed":{{"args":[{{"type":{{"resolved_path":{{"path":"timespec","id":{local_id},"args":null}}}}}}],"constraints":[]}}}}}},"items":[],"is_synthetic":false,"blanket_impl":null,"is_negative":false}}}}}}"#
        );
        let paths = format!(
            r#""93":{{"crate_id":1,"path":["core","cmp","PartialEq"],"kind":"trait"}},
                   "{local_id}":{{"crate_id":0,"path":["{krate}","timespec"],"kind":"struct"}}"#
        );
        shape_of(&doc(&[2], &index, &paths))
    };
    let reference = with_arg(2, "libc");
    let shim = with_arg(77, "libc_shim");
    assert!(diff(&reference, &shim).is_empty());
    let args: Vec<Option<String>> = reference
        .iter()
        .filter_map(|r| match r {
            ShapeRecord::Trait { args, .. } => Some(args.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        args,
        [Some(
            r#"{"angle_bracketed":{"args":[{"type":{"resolved_path":{"args":null,"path":"$crate::timespec"}}}],"constraints":[]}}"#
                .to_string()
        )]
    );
}

#[test]
fn a_function_without_its_body_bit_is_a_format_error() {
    let index = r#""1":{"id":1,"name":"f","inner":{"function":{"header":{}}}}"#;
    let parsed = Doc::parse(&doc(&[1], index, "")).expect("parses");
    let public = parsed.public().expect("public");
    assert!(public_shape(&parsed, &public, None).is_err());
}

#[test]
fn the_expected_surface_is_the_closure_plus_the_primitives_and_nothing_else() {
    let index = [
        function(1, "strlen", false, true, ""),
        function(2, "unlisted", false, true, ""),
        r#""3":{"id":3,"name":"c_int","inner":{"type_alias":{"type":{"primitive":"i32"}}}}"#.into(),
    ]
    .join(",");
    let parsed = Doc::parse(&doc(&[1, 2, 3], &index, "")).expect("parses");
    let public = parsed.public().expect("public");
    let expected = expected_shape(&parsed, &public, &["strlen".into()]).expect("expected");
    let items: Vec<&str> = expected
        .iter()
        .filter_map(|r| match r {
            ShapeRecord::Item { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(items, ["c_int", "strlen"]);
    assert_eq!(item_count(&expected), 2);
}
