// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `#[export]`, the attribute that marks what an aterm wasm module shows JS.
//! It replaces `#[wasm_bindgen]` on exports (docs/AUDIT-third-party-2026-10-15.md,
//! wasm-platform WP.6); use it through `aterm_wasm_abi::export`.
//!
//! * On `pub struct Name`: a JS class. JS holds a handle; the class gets a
//!   `free()` and a FinalizationRegistry entry.
//! * On `impl Name { .. }`: each plain `pub fn` becomes a method (`&self`,
//!   `&mut self`), a static method (no receiver), a getter
//!   (`#[export(getter)]`) or the constructor (`#[export(constructor)]`).
//!   Other items are left alone.
//! * On a module-level `pub fn`: a JS function (`#[export(js_name = x)]` to
//!   rename it).
//!
//! The source shape is wasm-bindgen's: a crate that exports only on wasm32
//! writes `#[cfg_attr(target_arch = "wasm32", export)]` on the item and
//! `#[cfg_attr(target_arch = "wasm32", export(getter))]` on a method. That
//! works because the impl's macro puts a marker attribute on each `pub fn`;
//! rustc expands a method's `cfg` and `cfg_attr` before it runs the marker,
//! which therefore sees the configured method and its options.
//!
//! No proc-macro2, quote or syn: the input is read as a plain token tree
//! (`tt.rs`), the expansion written as source (`expand.rs`). The item itself
//! passes through as the compiler's own tokens, so its spans, and every
//! diagnostic inside it, are where they were. String literals are read by
//! aterm-macro-support (`lit`); its cursor and derive-input parser are not
//! used: they read derive input, not impls and signatures, and run only
//! inside a macro invocation, where this parser is unit-tested natively.

use std::str::FromStr;

use proc_macro::{Delimiter, Group, TokenStream, TokenTree};

mod expand;
mod parse;
mod tt;

use parse::{Error, Item, Receiver};

/// Mark a struct, an inherent impl or a function as part of the module's JS
/// interface. See the crate docs.
#[proc_macro_attribute]
pub fn export(args: TokenStream, item: TokenStream) -> TokenStream {
    match export_item(args, item.clone()) {
        Ok(out) => out,
        Err(e) => with_error(item, &e),
    }
}

/// The per-method half of `#[export]` on an impl; the impl's expansion
/// writes it, with the impl's type as its argument. Not for direct use.
#[doc(hidden)]
#[proc_macro_attribute]
pub fn __export_method(args: TokenStream, item: TokenStream) -> TokenStream {
    match export_method(args, item.clone()) {
        Ok(out) => out,
        Err(e) => with_error(item, &e),
    }
}

/// The item unchanged, then the error: the item still compiles, so the
/// error is the only one reported.
fn with_error(item: TokenStream, e: &Error) -> TokenStream {
    let mut out = item;
    out.extend(parse(&format!("::core::compile_error!({:?});", e.0)));
    out
}

fn parse(src: &str) -> TokenStream {
    // The expansion is written by this crate; a lex failure is a bug here,
    // reported as a compile error rather than a panic.
    TokenStream::from_str(src).unwrap_or_else(|e| {
        TokenStream::from_str(&format!(
            "::core::compile_error!({:?});",
            format!("aterm-wasm-abi-macros wrote unlexable code: {e}")
        ))
        .unwrap_or_default()
    })
}

fn export_item(args: TokenStream, item: TokenStream) -> Result<TokenStream, Error> {
    let opts = parse::opts(&tt::from_stream(args))?;
    let tts = tt::from_stream(item.clone());
    match parse::item(&tts)? {
        Item::Struct { name, docs } => {
            if opts != parse::Opts::default() {
                return Err(Error("#[export] on a struct takes no options".into()));
            }
            let mut out = item;
            out.extend(parse(&expand::class(&name, &docs)));
            Ok(out)
        }
        Item::Impl { self_ty } => {
            if opts != parse::Opts::default() {
                return Err(Error("#[export] on an impl takes no options".into()));
            }
            Ok(mark_methods(item, &self_ty))
        }
        Item::Fn { docs, public, sig } => {
            if opts.getter || opts.constructor {
                return Err(Error(format!(
                    "`{}`: `getter` and `constructor` apply to a `pub fn` in an `#[export] impl`",
                    sig.name
                )));
            }
            if !public {
                return Err(Error(format!(
                    "`{}`: an exported function is plain `pub`",
                    sig.name
                )));
            }
            let js_name = opts.js_name.unwrap_or_else(|| sig.name.clone());
            let items = expand::export(&expand::Export {
                kind: expand::kind::FUNCTION,
                class: None,
                symbol: js_name.clone(),
                js_name,
                docs,
                sig: &sig,
            })?;
            let mut out = item;
            out.extend(parse(&format!("const _: () = {{\n{items}}};")));
            Ok(out)
        }
    }
}

/// The impl with `#[::aterm_wasm_abi::__export_method(Type)]` in front of
/// each plain `pub fn`.
fn mark_methods(item: TokenStream, self_ty: &[tt::Tt]) -> TokenStream {
    let marker = parse(&format!(
        "#[::aterm_wasm_abi::__export_method({})]",
        tt::to_source(self_ty)
    ));
    let mut top: Vec<TokenTree> = item.into_iter().collect();
    if let Some(TokenTree::Group(body)) = top.last_mut()
        && body.delimiter() == Delimiter::Brace
    {
        let tokens: Vec<TokenTree> = body.stream().into_iter().collect();
        let starts = parse::impl_pub_fns(&tt::from_stream(body.stream()));
        let mut stream = TokenStream::new();
        for (i, t) in tokens.into_iter().enumerate() {
            if starts.contains(&i) {
                stream.extend(marker.clone());
            }
            stream.extend([t]);
        }
        let mut group = Group::new(Delimiter::Brace, stream);
        group.set_span(body.span());
        *body = group;
    }
    top.into_iter().collect()
}

fn export_method(args: TokenStream, item: TokenStream) -> Result<TokenStream, Error> {
    let class_ty = tt::from_stream(args);
    let Some(tt::Tt::Ident(class)) = class_ty
        .iter()
        .rev()
        .find(|t| matches!(t, tt::Tt::Ident(_)))
        .cloned()
    else {
        return Err(Error("__export_method: expected the impl's type".into()));
    };
    let tts = tt::from_stream(item.clone());
    let (attrs, _) = parse::attrs(&tts);
    let mut opts = parse::Opts::default();
    let mut drop_attrs = Vec::new();
    for (at, a) in &attrs {
        if let Some(o) = parse::export_attr(a) {
            let o = o?;
            opts.getter |= o.getter;
            opts.constructor |= o.constructor;
            opts.js_name = o.js_name.or(opts.js_name);
            drop_attrs.extend([*at, at + 1]);
        }
    }
    let docs = parse::docs(&attrs);
    let Item::Fn { sig, .. } = parse::item(&tts)? else {
        return Err(Error("__export_method: expected a method".into()));
    };
    let kind = if opts.constructor {
        if sig.receiver != Receiver::None {
            return Err(Error(format!(
                "`{}`: a constructor takes no `self`",
                sig.name
            )));
        }
        expand::kind::CONSTRUCTOR
    } else if opts.getter {
        if sig.receiver == Receiver::None || !sig.params.is_empty() || sig.ret.is_none() {
            return Err(Error(format!(
                "`{}`: a getter is `fn(&self) -> T`",
                sig.name
            )));
        }
        expand::kind::GETTER
    } else if sig.receiver == Receiver::None {
        expand::kind::STATIC
    } else {
        expand::kind::METHOD
    };
    let items = expand::export(&expand::Export {
        kind,
        class: Some((&class_ty, &class)),
        js_name: opts.js_name.unwrap_or_else(|| sig.name.clone()),
        symbol: expand::member_symbol(&class, &sig.name),
        docs,
        sig: &sig,
    })?;

    // The method as written, less its `#[export(..)]` attributes, with the
    // export's items at the top of its body.
    let mut out: Vec<TokenTree> = Vec::new();
    let tokens: Vec<TokenTree> = item.into_iter().collect();
    let last = tokens.len().saturating_sub(1);
    for (i, t) in tokens.into_iter().enumerate() {
        if drop_attrs.contains(&i) {
            continue;
        }
        match t {
            TokenTree::Group(body) if i == last && body.delimiter() == Delimiter::Brace => {
                let mut inner: Vec<TokenTree> = body.stream().into_iter().collect();
                // Inner attributes (`#![..]`) stay first.
                let mut keep = 0;
                while matches!(inner.get(keep), Some(TokenTree::Punct(p)) if p.as_char() == '#')
                    && matches!(inner.get(keep + 1), Some(TokenTree::Punct(p)) if p.as_char() == '!')
                {
                    keep += 3;
                }
                let rest = inner.split_off(keep.min(inner.len()));
                let mut stream: TokenStream = inner.into_iter().collect();
                stream.extend(parse(&items));
                stream.extend(rest);
                let mut group = Group::new(Delimiter::Brace, stream);
                group.set_span(body.span());
                out.push(TokenTree::Group(group));
            }
            t => out.push(t),
        }
    }
    Ok(out.into_iter().collect())
}
