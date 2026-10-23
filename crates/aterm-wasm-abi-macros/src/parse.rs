// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Parser for `#[export]` attributes, items, and function signatures.

use crate::tt::{Delim, Tt, str_value, to_source};

/// A message for `compile_error!`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl Error {
    fn new(msg: impl Into<String>) -> Self {
        Error(msg.into())
    }
}

/// `#[export(...)]`'s arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Opts {
    /// `getter`: a read-only property.
    pub getter: bool,
    /// `constructor`: the class's `constructor`.
    pub constructor: bool,
    /// `js_name = name`: the JS name, when not the Rust one.
    pub js_name: Option<String>,
}

/// The receiver of a method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Receiver {
    /// Free function, static method, or constructor.
    None,
    /// `&self`.
    Shared,
    /// `&mut self`.
    Exclusive,
}

/// One parameter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    /// Binding name without `r#` (`argN` for `_`).
    pub name: String,
    /// The type; for a `&T` parameter, `T`.
    pub ty: Vec<Tt>,
    /// Whether the parameter is `&T`.
    pub by_ref: bool,
}

/// A function signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sig {
    /// The Rust name.
    pub name: String,
    /// `&self`, `&mut self` or none.
    pub receiver: Receiver,
    /// The parameters after the receiver.
    pub params: Vec<Param>,
    /// Return type (`None` for `()`).
    pub ret: Option<Vec<Tt>>,
}

/// What `#[export]` was applied to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    /// `struct Name`: a class.
    Struct {
        /// The struct's name.
        name: String,
        /// Its doc comment.
        docs: String,
    },
    /// `impl Type { .. }`.
    Impl {
        /// The type, as written.
        self_ty: Vec<Tt>,
    },
    /// A function.
    Fn {
        /// Its doc comment.
        docs: String,
        /// Whether visibility is plain `pub`.
        public: bool,
        /// Its signature.
        sig: Sig,
    },
}

/// Leading `#[...]` attributes of `tts` and the token index after the last.
pub fn attrs(tts: &[Tt]) -> (Vec<(usize, &[Tt])>, usize) {
    let mut out = Vec::new();
    let mut i = 0;
    while let (Some(t), Some(Tt::Group(Delim::Bracket, inner))) = (tts.get(i), tts.get(i + 1)) {
        if !t.is_punct('#') {
            break;
        }
        out.push((i, inner.as_slice()));
        i += 2;
    }
    (out, i)
}

/// Doc comment assembled from `#[doc = "..."]` attributes in `attrs`.
pub fn docs(attrs: &[(usize, &[Tt])]) -> String {
    let mut lines = Vec::new();
    for (_, a) in attrs {
        if let [Tt::Ident(d), eq, Tt::Lit(lit)] = a
            && d == "doc"
            && eq.is_punct('=')
            && let Some(text) = str_value(lit)
        {
            lines.push(text);
        }
    }
    lines.join("\n")
}

/// Parse `attr` if it is an `export` or `export(...)` attribute.
///
/// # Errors
/// Unknown or malformed options.
pub fn export_attr(attr: &[Tt]) -> Option<Result<Opts, Error>> {
    let (path, args) = match attr.split_last() {
        Some((Tt::Group(Delim::Paren, args), path)) => (path, args.as_slice()),
        _ => (attr, &[][..]),
    };
    let last = path.iter().rev().find_map(|t| match t {
        Tt::Ident(i) => Some(i.as_str()),
        _ => None,
    })?;
    let is_path = path
        .iter()
        .all(|t| matches!(t, Tt::Ident(_)) || t.is_punct(':'));
    (last == "export" && is_path).then(|| opts(args))
}

/// Parse `#[export(...)]` options from argument tokens.
///
/// # Errors
/// Anything other than `getter`, `constructor`, or `js_name = <name>`; also both `getter` and
/// `constructor`.
pub fn opts(args: &[Tt]) -> Result<Opts, Error> {
    let mut out = Opts::default();
    for arg in split_commas(args) {
        match arg {
            [] => {}
            [t] if t.is_ident("getter") => out.getter = true,
            [t] if t.is_ident("constructor") => out.constructor = true,
            [k, eq, v] if k.is_ident("js_name") && eq.is_punct('=') => {
                out.js_name = Some(match v {
                    Tt::Ident(i) => i.clone(),
                    Tt::Lit(l) => str_value(l)
                        .ok_or_else(|| Error::new("`js_name` takes a name or a string"))?,
                    _ => return Err(Error::new("`js_name` takes a name or a string")),
                });
            }
            other => {
                return Err(Error::new(format!(
                    "unknown #[export] option `{}`: expected `getter`, `constructor` or \
                     `js_name = name`",
                    to_source(other)
                )));
            }
        }
    }
    if out.getter && out.constructor {
        return Err(Error::new(
            "#[export] cannot be both `getter` and `constructor`",
        ));
    }
    Ok(out)
}

/// Parse the item `tts` starts with.
///
/// # Errors
/// Unsupported item kinds or constructs.
pub fn item(tts: &[Tt]) -> Result<Item, Error> {
    let (attrs, mut i) = attrs(tts);
    let docs = docs(&attrs);
    let public = tts.get(i).is_some_and(|t| t.is_ident("pub"))
        && !matches!(tts.get(i + 1), Some(Tt::Group(Delim::Paren, _)));
    if tts.get(i).is_some_and(|t| t.is_ident("pub")) {
        i += 1;
        if matches!(tts.get(i), Some(Tt::Group(Delim::Paren, _))) {
            i += 1;
        }
    }
    let rest = &tts[i..];
    match rest.first() {
        Some(t) if t.is_ident("struct") => {
            let Some(Tt::Ident(name)) = rest.get(1) else {
                return Err(Error::new("#[export] struct: expected a name"));
            };
            if rest.get(2).is_some_and(|t| t.is_punct('<')) {
                return Err(Error::new(format!(
                    "#[export] struct {name}: a generic struct cannot be a JS class"
                )));
            }
            Ok(Item::Struct {
                name: name.clone(),
                docs,
            })
        }
        Some(t) if t.is_ident("impl") => {
            if rest.get(1).is_some_and(|t| t.is_punct('<')) {
                return Err(Error::new(
                    "#[export] impl: a generic impl cannot be exported",
                ));
            }
            let Some((Tt::Group(Delim::Brace, _), ty)) = rest[1..].split_last() else {
                return Err(Error::new("#[export] impl: expected `impl Type { .. }`"));
            };
            if ty.iter().any(|t| t.is_ident("for")) {
                return Err(Error::new(
                    "#[export] impl: only an inherent impl (`impl Type`) is exported, not a \
                     trait impl",
                ));
            }
            if ty.is_empty() {
                return Err(Error::new("#[export] impl: expected a type"));
            }
            Ok(Item::Impl {
                self_ty: ty.to_vec(),
            })
        }
        _ => Ok(Item::Fn {
            docs,
            public,
            sig: sig(rest)?,
        }),
    }
}

/// Parse the function signature in `tts` (after attributes and visibility).
///
/// # Errors
/// Unsupported qualifiers, generics, receivers, patterns, or parameter/return types.
pub fn sig(tts: &[Tt]) -> Result<Sig, Error> {
    let mut i = 0;
    while let Some(t) = tts.get(i) {
        match t {
            Tt::Ident(q) if q == "const" => i += 1,
            Tt::Ident(q) if q == "async" || q == "unsafe" || q == "extern" => {
                return Err(Error::new(format!("an exported function cannot be `{q}`")));
            }
            _ => break,
        }
    }
    if !tts.get(i).is_some_and(|t| t.is_ident("fn")) {
        return Err(Error::new(
            "#[export] takes a `struct`, an inherent `impl` or a `pub fn`",
        ));
    }
    let Some(Tt::Ident(name)) = tts.get(i + 1) else {
        return Err(Error::new("expected a function name"));
    };
    let fail = |why: &str| Error::new(format!("cannot export `{name}`: {why}"));
    let mut i = i + 2;
    if tts.get(i).is_some_and(|t| t.is_punct('<')) {
        return Err(fail("a generic function has no single JS signature"));
    }
    let Some(Tt::Group(Delim::Paren, params)) = tts.get(i) else {
        return Err(fail("expected its parameter list"));
    };
    i += 1;
    let mut ret = None;
    if tts.get(i).is_some_and(|t| t.is_punct('-'))
        && tts.get(i + 1).is_some_and(|t| t.is_punct('>'))
    {
        let start = i + 2;
        let mut end = start;
        while let Some(t) = tts.get(end) {
            if matches!(t, Tt::Group(Delim::Brace, _)) || t.is_ident("where") {
                break;
            }
            end += 1;
        }
        let ty = &tts[start..end];
        check_type(ty).map_err(|why| fail(&why))?;
        ret = Some(ty.to_vec());
        i = end;
    }
    if tts.get(i).is_some_and(|t| t.is_ident("where")) {
        return Err(fail("a `where` clause"));
    }
    if !matches!(tts.get(i), Some(Tt::Group(Delim::Brace, _))) || i + 1 != tts.len() {
        return Err(fail("expected its body"));
    }

    let mut receiver = Receiver::None;
    let mut out = Vec::new();
    for (n, p) in split_commas(params).into_iter().enumerate() {
        if p.is_empty() {
            continue;
        }
        if n == 0
            && let Some(r) = self_param(p).map_err(|why| fail(&why))?
        {
            receiver = r;
            continue;
        }
        if p.first().is_some_and(|t| t.is_punct('#')) {
            return Err(fail("an attribute on a parameter"));
        }
        let colon = p
            .iter()
            .position(|t| t.is_punct(':'))
            .ok_or_else(|| fail("a parameter without a type"))?;
        let name = match &p[..colon] {
            [Tt::Ident(id)] if id == "_" => format!("arg{}", out.len()),
            [Tt::Ident(id)] | [Tt::Ident(_), Tt::Ident(id)]
                if id != "self" && (colon == 1 || p[0].is_ident("mut")) =>
            {
                id.strip_prefix("r#").unwrap_or(id).to_string()
            }
            pat => {
                return Err(fail(&format!(
                    "the pattern `{}`: name each parameter",
                    to_source(pat)
                )));
            }
        };
        let ty = &p[colon + 1..];
        check_type(ty).map_err(|why| fail(&why))?;
        let (ty, by_ref) = match ty {
            [amp, rest @ ..] if amp.is_punct('&') => {
                let rest = match rest {
                    [q, Tt::Ident(_), rest @ ..] if q.is_punct('\'') => rest,
                    _ => rest,
                };
                if rest.first().is_some_and(|t| t.is_ident("mut")) {
                    return Err(fail(&format!(
                        "`{name}: &mut ..`: a JS value cannot be borrowed mutably; take it by \
                         value and return the result"
                    )));
                }
                (rest.to_vec(), true)
            }
            _ => (ty.to_vec(), false),
        };
        out.push(Param { name, ty, by_ref });
    }
    Ok(Sig {
        name: name.clone(),
        receiver,
        params: out,
        ret,
    })
}

fn self_param(p: &[Tt]) -> Result<Option<Receiver>, String> {
    let without_lifetime: Vec<&Tt> = {
        let mut v = Vec::new();
        let mut i = 0;
        while i < p.len() {
            if p[i].is_punct('\'') {
                i += 2;
                continue;
            }
            v.push(&p[i]);
            i += 1;
        }
        v
    };
    match without_lifetime.as_slice() {
        [a, s] if a.is_punct('&') && s.is_ident("self") => Ok(Some(Receiver::Shared)),
        [a, m, s] if a.is_punct('&') && m.is_ident("mut") && s.is_ident("self") => {
            Ok(Some(Receiver::Exclusive))
        }
        [s, ..] | [_, s, ..] if s.is_ident("self") => Err(
            "`self` by value or with a type: an exported method takes `&self` or `&mut self`"
                .to_string(),
        ),
        _ => Ok(None),
    }
}

fn check_type(ty: &[Tt]) -> Result<(), String> {
    if ty.is_empty() {
        return Err("an empty type".to_string());
    }
    if ty
        .first()
        .is_some_and(|t| t.is_ident("impl") || t.is_ident("dyn"))
    {
        return Err(format!("`{}`: name a concrete type", to_source(ty)));
    }
    if ty.first().is_some_and(|t| t.is_punct('!')) {
        return Err("`!`: an export returns".to_string());
    }
    Ok(())
}

/// Split `tts` at top-level commas, respecting `<..>` nesting (ignoring `->`).
pub fn split_commas(tts: &[Tt]) -> Vec<&[Tt]> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, t) in tts.iter().enumerate() {
        match t {
            Tt::Punct('<', _) => depth += 1,
            Tt::Punct('>', _) if !(i > 0 && tts[i - 1] == Tt::Punct('-', true)) => depth -= 1,
            Tt::Punct(',', _) if depth == 0 => {
                out.push(&tts[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&tts[start..]);
    out
}

/// Token start indices of each plain `pub fn` item in an impl body.
pub fn impl_pub_fns(body: &[Tt]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let start = i;
        while body.get(i).is_some_and(|t| t.is_punct('#'))
            && matches!(body.get(i + 1), Some(Tt::Group(Delim::Bracket, _)))
        {
            i += 2;
        }
        let public = body.get(i).is_some_and(|t| t.is_ident("pub"))
            && !matches!(body.get(i + 1), Some(Tt::Group(Delim::Paren, _)));
        if body.get(i).is_some_and(|t| t.is_ident("pub")) {
            i += 1;
            if matches!(body.get(i), Some(Tt::Group(Delim::Paren, _))) {
                i += 1;
            }
        }
        let mut j = i;
        while body.get(j).is_some_and(|t| {
            matches!(t, Tt::Lit(_))
                || ["const", "async", "unsafe", "extern", "default"]
                    .iter()
                    .any(|q| t.is_ident(q))
        }) {
            j += 1;
        }
        let is_fn = body.get(j).is_some_and(|t| t.is_ident("fn"));
        let is_macro = matches!(body.get(i), Some(Tt::Ident(_)))
            && body.get(i + 1).is_some_and(|t| t.is_punct('!'));
        let mut k = i;
        while let Some(t) = body.get(k) {
            if t.is_punct(';') || (matches!(t, Tt::Group(Delim::Brace, _)) && (is_fn || is_macro)) {
                break;
            }
            k += 1;
        }
        // A brace-delimited macro call may still end in `;`.
        if is_macro && body.get(k + 1).is_some_and(|t| t.is_punct(';')) {
            k += 1;
        }
        if is_fn && public {
            starts.push(start);
        }
        i = k + 1;
    }
    starts
}

#[cfg(test)]
mod tests {
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
}
