// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

const SRC: &str = "pub struct A;\n\
    impl A {\n    /// Doc.\n    pub const fn level(self) -> u8 {\n        match self { _ => 1, }\n    }\n}\n\
    impl From<bool> for A {\n    fn from(b: bool) -> Self { let _ = \"x  y\"; A }\n}\n\
    pub fn level(x: u8) -> u8 { x }\n\
    #[cfg(test)]\nmod tests {\n    fn level() {}\n    fn only_here() {}\n}\n\
    macro_rules! m {\n    () => { fn in_macro() {} };\n}\n";

fn toks(src: &str, path: &str) -> String {
    item_tokens(src, path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn an_item_is_found_by_its_qualifiers() {
    assert_eq!(
        toks(SRC, "A::level"),
        "impl A { const fn level ( self ) -> u8 { match self { _ => 1 } }"
    );
    assert_eq!(toks(SRC, "level"), "fn level ( x : u8 ) -> u8 { x }");
    assert!(item_tokens(SRC, "<A as From<bool>>::from").is_ok());
    assert!(item_tokens(SRC, "<A as Into<bool>>::from").is_err());
    assert!(item_tokens(SRC, "B::level").is_err());
    assert_eq!(toks(SRC, "A"), "struct A ;");
}

#[test]
fn test_code_and_macros_hold_no_item() {
    assert!(item_tokens(SRC, "only_here").is_err());
    assert!(item_tokens(SRC, "in_macro").is_err());
    let gone = SRC.replace("pub fn level(x: u8) -> u8 { x }\n", "");
    assert!(
        item_tokens(
            &gone.replace("fn level() {}", "fn level(x: u8) -> u8 { x }"),
            "level"
        )
        .is_err(),
        "a test module's copy never stands in for a deleted function"
    );
}

#[test]
fn operators_are_whole_tokens() {
    let and = "fn f(a: bool, b: bool) -> bool { a && b }";
    let amp = "fn f(a: bool, b: bool) -> bool { a & &b }";
    assert_ne!(toks(and, "f"), toks(amp, "f"));
    assert_eq!(
        toks("fn g(x: u8) -> u8 { x >>= 1; x..=2 }", "g"),
        "fn g ( x : u8 ) -> u8 { x >>= 1 ; x ..= 2 }"
    );
    assert_eq!(
        toks("fn h<'a>(x: &'a str) {}", "h"),
        "fn h < 'a > ( x : & 'a str ) { }"
    );
}

#[test]
fn a_trailing_comma_goes_but_a_one_tuple_keeps_its_comma() {
    let tuple = "fn f(x: u8) -> (u8,) { (x,) }";
    let paren = "fn f(x: u8) -> (u8) { (x) }";
    assert_ne!(toks(tuple, "f"), toks(paren, "f"));
    assert_eq!(
        toks(
            "fn f(\n    x: u8,\n) -> u8 {\n    g(\n        x,\n    )\n}",
            "f"
        ),
        toks("fn f(x: u8) -> u8 { g(x) }", "f")
    );
    assert_eq!(
        toks("fn f() -> [u8; 2] { [1, 2,] }", "f"),
        toks("fn f() -> [u8; 2] { [1, 2] }", "f")
    );
    assert_ne!(
        toks("fn f() { g(a, b) }", "f"),
        toks("fn f() { g(a b) }", "f"),
        "only a comma before a closing bracket is a formatter's"
    );
}

#[test]
fn a_brace_in_the_signature_does_not_open_the_body() {
    let src = "fn f() -> Foo<{ N }> { 1 }\n";
    let edited = "fn f() -> Foo<{ N }> { 2 }\n";
    assert_ne!(toks(src, "f"), toks(edited, "f"));
    assert_eq!(toks(src, "f"), "fn f ( ) -> Foo < { N } > { 1 }");
    let arr = "fn f(x: [u8; { 2 }]) { 1 }\n";
    assert_eq!(toks(arr, "f"), "fn f ( x : [ u8 ; { 2 } ] ) { 1 }");
}

#[test]
fn attributes_and_the_impl_head_are_part_of_the_item() {
    let src = "impl A {\n    pub(crate) fn f(self) -> u8 { 1 }\n}\n";
    let gated = "impl A {\n    #[cfg(any())]\n    pub(crate) fn f(self) -> u8 { 1 }\n}\n";
    let moved = "impl<T> A<T> {\n    pub(crate) fn f(self) -> u8 { 1 }\n}\n";
    assert_ne!(toks(src, "A::f"), toks(gated, "A::f"));
    assert_ne!(toks(src, "A::f"), toks(moved, "A::f"));
    assert_eq!(
        toks(src, "A::f"),
        toks("impl A {\n    pub fn f(self) -> u8 { 1 }\n}\n", "A::f"),
        "visibility is not behaviour"
    );
}

#[test]
fn constants_and_enums_are_items() {
    let src = "pub const MAX: usize = 512;\n#[repr(usize)]\npub enum Level {\n    Error = 1,\n    Warn = 2,\n}\nconst fn f<const N: usize>() {}\n";
    assert_eq!(toks(src, "MAX"), "const MAX : usize = 512 ;");
    assert_eq!(
        toks(src, "Level"),
        "# [ repr ( usize ) ] enum Level { Error = 1 , Warn = 2 }"
    );
    assert_ne!(
        toks(src, "Level"),
        toks(
            &src.replace("Error = 1,\n    Warn = 2", "Warn = 2,\n    Error = 1"),
            "Level"
        ),
        "the variant order is the derived order"
    );
    assert!(item_tokens(src, "N").is_err(), "a const generic is no item");
    assert_eq!(item_names(src), ["MAX", "Level", "f"]);
    assert_eq!(toks(src, "f"), "const fn f < const N : usize > ( ) { }");
}
