// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::parse::{Item, item};
use crate::tt::lex;
use aterm_wasm_abi::descriptor as d;

#[test]
fn the_record_constants_are_the_runtimes() {
    assert_eq!(
        [
            kind::CLASS,
            kind::FUNCTION,
            kind::METHOD,
            kind::STATIC,
            kind::GETTER,
            kind::CONSTRUCTOR
        ],
        [
            d::kind::CLASS,
            d::kind::FUNCTION,
            d::kind::METHOD,
            d::kind::STATIC,
            d::kind::GETTER,
            d::kind::CONSTRUCTOR
        ]
    );
    assert_eq!(SECTION, d::SECTION);
}

#[test]
fn a_written_head_decodes_with_the_runtimes_reader() {
    let mut body = head(
        kind::METHOD,
        "Term",
        "encode",
        "term_encode",
        " Docs —\n two.",
    );
    body.extend([2u8, 2]);
    enc_str(&mut body, "key");
    body.extend(<str as aterm_wasm_abi::__rt::Describe>::DESC.as_bytes());
    enc_str(&mut body, "n");
    body.extend(<Option<u64> as aterm_wasm_abi::__rt::Describe>::DESC.as_bytes());
    body.extend(<Result<Vec<u8>, String> as aterm_wasm_abi::__rt::Describe>::DESC.as_bytes());
    let mut rec = d::MAGIC.to_vec();
    rec.push(d::VERSION);
    rec.extend((body.len() as u32).to_le_bytes());
    rec.extend(body);
    let r = &d::decode(&rec).unwrap()[0];
    assert_eq!(
        (
            r.kind,
            r.class.as_str(),
            r.js_name.as_str(),
            r.symbol.as_str()
        ),
        (d::kind::METHOD, "Term", "encode", "term_encode")
    );
    assert_eq!(r.docs, " Docs —\n two.");
    assert_eq!(r.receiver, d::receiver::EXCLUSIVE);
    assert_eq!(
        r.params,
        [
            ("key".to_string(), d::Ty::Str),
            ("n".to_string(), d::Ty::Option(Box::new(d::Ty::U64)))
        ]
    );
    assert_eq!(
        r.ret,
        d::Ty::Result(
            Box::new(d::Ty::Vec(Box::new(d::Ty::U8))),
            Box::new(d::Ty::String)
        )
    );
}

fn sig(src: &str) -> Sig {
    match item(&lex(src)).unwrap() {
        Item::Fn { sig, .. } => sig,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_method_expands_to_a_borrowing_export() {
    let s = sig("pub fn find(&mut self, q: &str, n: Option<u32>) -> Result<Self, String> { x }");
    let ty = lex("Term");
    let out = export(&Export {
        kind: kind::METHOD,
        class: Some((&ty, "Term")),
        js_name: "find".into(),
        symbol: member_symbol("Term", "find"),
        docs: String::new(),
        sig: &s,
    })
    .unwrap();
    let tts = lex(&out);
    assert!(!tts.is_empty());
    assert!(out.contains("export_name = \"term_find\""));
    assert!(out.contains("borrow_mut::<Term>(__this)"));
    assert!(out.contains("<Term>::find(&mut *__this, &*__p0, __p1)"));
    assert!(out.contains("<str as ::aterm_wasm_abi::__rt::RefFromAbi>"));
    assert!(out.contains("Result<Term, String>") || out.contains("Result < Term , String >"));
    assert!(!out.contains("Self"));

    let shared = sig("pub fn len(&self) -> u32 { 0 }");
    let out = export(&Export {
        kind: kind::GETTER,
        class: Some((&ty, "Term")),
        js_name: "len".into(),
        symbol: member_symbol("Term", "len"),
        docs: String::new(),
        sig: &shared,
    })
    .unwrap();
    assert!(out.contains("let __this = unsafe { ::aterm_wasm_abi::__rt::borrow::<Term>"));

    let free = sig("pub fn method(&self) {}");
    assert!(
        export(&Export {
            kind: kind::FUNCTION,
            class: None,
            js_name: "method".into(),
            symbol: "method".into(),
            docs: String::new(),
            sig: &free,
        })
        .is_err(),
        "a free function cannot take self"
    );
}

#[test]
fn a_class_expands_to_impls_a_free_export_and_a_record() {
    let out = class("LinkHit", " A link.");
    assert!(!lex(&out).is_empty());
    assert!(out.contains("__class!(LinkHit)"));
    assert!(out.contains("export_name = \"__aterm_free_linkhit\""));
    assert!(out.contains("free_export::<LinkHit>(ptr, from_finalizer)"));
}
