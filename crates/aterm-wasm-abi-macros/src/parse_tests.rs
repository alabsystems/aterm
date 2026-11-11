// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::tt::lex;

fn sig_of(src: &str) -> Result<Sig, Error> {
    let tts = lex(src);
    match item(&tts)? {
        Item::Fn { sig, .. } => Ok(sig),
        other => panic!("not a fn: {other:?}"),
    }
}

fn src(ty: &[Tt]) -> String {
    to_source(ty)
}

#[test]
fn receivers_and_parameters_parse_with_nested_generics() {
    let s = sig_of(
        "pub fn encode_key(&mut self, key: &str, mods: u8, base: Option<char>, \
         map: Result<Option<Vec<u8>>, String>, mut n: u32, _: f64, r#type: u8, \
         mut r#in: u8) -> Option<Vec<u8>> { None }",
    )
    .unwrap();
    assert_eq!(s.name, "encode_key");
    assert_eq!(s.receiver, Receiver::Exclusive);
    let got: Vec<(String, String, bool)> = s
        .params
        .iter()
        .map(|p| (p.name.clone(), src(&p.ty), p.by_ref))
        .collect();
    let want = [
        ("key", "str", true),
        ("mods", "u8", false),
        ("base", "Option<char>", false),
        ("map", "Result<Option<Vec<u8>>, String>", false),
        ("n", "u32", false),
        ("arg5", "f64", false),
        ("type", "u8", false),
        ("in", "u8", false),
    ];
    assert_eq!(got.len(), want.len());
    for ((n, t, r), (wn, wt, wr)) in got.iter().zip(want) {
        assert_eq!((n.as_str(), r), (wn, &wr));
        assert_eq!(lex(t), lex(wt), "{n}");
    }
    assert_eq!(lex(&src(s.ret.as_ref().unwrap())), lex("Option<Vec<u8>>"));

    let s = sig_of("pub fn width(&'a self) -> u32 { 0 }").unwrap();
    assert_eq!((s.receiver, s.params.len()), (Receiver::Shared, 0));
    let s = sig_of("pub const fn new(bytes: &'a [u8],) -> Result<Self, String> { x }").unwrap();
    assert_eq!(s.receiver, Receiver::None);
    assert_eq!(lex(&src(&s.params[0].ty)), lex("[u8]"));
    assert!(s.params[0].by_ref);
    assert_eq!(sig_of("pub fn unit() {}").unwrap().ret, None);
    let f = sig_of("pub fn f(cb: fn(u32) -> u32) -> u8 { 0 }").unwrap();
    assert_eq!(
        lex(&src(&f.params[0].ty)),
        lex("fn(u32) -> u32"),
        "`->` closes no `<`"
    );
}

#[test]
fn what_cannot_cross_is_refused_by_name() {
    for (code, why) in [
        ("pub fn f<T>(x: T) {}", "generic"),
        ("pub async fn f() {}", "async"),
        ("pub unsafe fn f() {}", "unsafe"),
        ("pub fn f(self) {}", "self` by value"),
        ("pub fn f(mut self) {}", "self` by value"),
        ("pub fn f(self: Box<Self>) {}", "self` by value"),
        ("pub fn f(&self, b: &mut [u8]) {}", "borrowed mutably"),
        ("pub fn f((a, b): (u32, u32)) {}", "pattern"),
        ("pub fn f(x: impl Into<u32>) {}", "concrete type"),
        ("pub fn f() -> ! { loop {} }", "an export returns"),
        ("pub fn f() where u8: Copy {}", "where"),
        ("pub struct S<T>(T);", "generic struct"),
        ("impl<T> S<T> {}", "generic impl"),
        ("impl Drop for S {}", "trait impl"),
        ("pub enum E {}", "takes a `struct`"),
    ] {
        let tts = lex(code);
        let err = item(&tts).expect_err(code);
        assert!(err.0.contains(why), "{code}: {}", err.0);
    }
}

#[test]
fn docs_and_options_come_from_the_attributes() {
    let tts = lex("#[doc = \" First line.\"] #[doc = r\"\"] #[must_use] \
         #[doc = \" A \\\"quoted\\\" — line.\"] pub struct LinkHit { url: String }");
    assert_eq!(
        item(&tts).unwrap(),
        Item::Struct {
            name: "LinkHit".into(),
            docs: " First line.\n\n A \"quoted\" — line.".into(),
        }
    );
    assert_eq!(
        opts(&lex("getter")).unwrap(),
        Opts {
            getter: true,
            ..Opts::default()
        }
    );
    assert_eq!(
        opts(&lex("js_name = atermSmokePanic"))
            .unwrap()
            .js_name
            .as_deref(),
        Some("atermSmokePanic")
    );
    assert_eq!(
        opts(&lex("js_name = r#type")).unwrap().js_name.as_deref(),
        Some("type")
    );
    assert_eq!(split_commas(&lex("a > b, c")).len(), 2);
    assert_eq!(
        opts(&lex("constructor, js_name = \"make\"")).unwrap(),
        Opts {
            constructor: true,
            js_name: Some("make".into()),
            ..Opts::default()
        }
    );
    assert!(opts(&lex("setter")).is_err());
    assert!(opts(&lex("getter, constructor")).is_err());
    assert!(export_attr(&lex("export(getter)")).is_some());
    assert!(export_attr(&lex("aterm_wasm_abi::export")).is_some());
    assert!(export_attr(&lex("doc = \" export\"")).is_none());
    assert!(export_attr(&lex("cfg_attr(x, export)")).is_none());
}

#[test]
fn only_plain_pub_fns_of_an_impl_get_a_marker() {
    let body = lex("#[doc = \" a\"] pub fn a(&self) {} \
         fn private(&self) {} \
         pub(crate) fn scoped(&self) {} \
         pub const LIMIT: u32 = { 3 }; \
         #[cfg(debug_assertions)] pub const fn b() -> u8 { 1 } \
         type T = u8; \
         m! { pub fn not_this() {} } \
         pub fn c(&mut self, f: fn() -> u8) -> u8 { f() }");
    let starts = impl_pub_fns(&body);
    let names: Vec<String> = starts
        .iter()
        .map(|&s| {
            let (_, i) = attrs(&body[s..]);
            let rest = &body[s + i..];
            let at = rest.iter().position(|t| t.is_ident("fn")).unwrap();
            src(&rest[at + 1..at + 2])
        })
        .collect();
    assert_eq!(names, ["a", "b", "c"]);
}
