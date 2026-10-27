// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Proc macros for TLA+ specification refinement attributes (`aterm-macro-support` on std `proc_macro`).

mod item;
mod model;
mod refusals;

use aterm_macro_support::{Cursor, Error, lit, q};
use proc_macro::{Ident, Literal, Span, TokenStream, TokenTree};

use crate::item::FnItem;

/// What an anchor on anything but a fn with a body expands to: a compile error, then the item
/// as written (so nothing else fails for its loss). The record is an item inside the body; beside
/// a trait's method declaration it would make the trait not dyn-compatible, and the
/// `macro_rules!` attributes on the Trust compiler refuse the item in these words.
fn refusal(item: &FnItem) -> TokenStream {
    let mut out = Error::new(
        Span::call_site(),
        "an anchor goes on a fn with a body: the record is an item inside it",
    )
    .to_compile_error();
    out.extend(item.stream());
    out
}

/// The execution-evidence key for one anchor: `"<machine>::<action> @ <rust_method>"`.
fn entry_id_for(machine: &str, action: &str, rust_method: &str) -> String {
    format!("{machine}::{action} @ {rust_method}")
}

/// The entry probe, a fn body's first statement once the record is in place.
fn entry_probe(entry_id: &str) -> TokenStream {
    q!(
        "::aterm_spec_anchor::xref::note_entered(#id);",
        id = Literal::string(entry_id)
    )
}

/// The annotated fn with its anchor `record` (a registry submit, an anonymous const item) and
/// `probe` first in its body, where a method of a trait impl can hold them (the impl takes no
/// item the trait does not declare); the refusal for anything else. A clippy lint that judges a
/// body's shape (`manual_async_fn`, say) can miss an anchored fn: it reads the body with them.
fn anchored(item: &FnItem, record: TokenStream, probe: TokenStream) -> TokenStream {
    let mut prelude = record;
    prelude.extend(probe);
    item.with_prelude(&prelude).unwrap_or_else(|| refusal(item))
}

/// The `key = "value"` list an anchor attribute takes, each key one of `keys`. A key
/// written twice keeps its last value.
struct Keys(Vec<(&'static str, String)>);

impl Keys {
    fn parse(attr: TokenStream, keys: &[&'static str]) -> Result<Self, Error> {
        let mut c = Cursor::new(attr, Span::call_site());
        let mut found: Vec<(&'static str, String)> = Vec::new();
        while !c.is_empty() {
            let ident = model::expect_name(&mut c, "identifier")?;
            c.expect_punct('=', "`=`")?;
            let value = expect_str(&mut c)?;
            let name = ident.to_string();
            let Some(key) = keys.iter().find(|k| **k == name) else {
                return Err(Error::new(ident.span(), format!("unknown key: {name}")));
            };
            found.retain(|(k, _)| k != key);
            found.push((key, value));
            if !c.is_empty() {
                c.expect_punct(',', "`,`")?;
            }
        }
        Ok(Keys(found))
    }

    fn get(&self, key: &str) -> Option<String> {
        self.0
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.clone())
    }

    /// The value of a key the attribute must carry.
    fn require(&self, key: &str) -> Result<String, Error> {
        self.get(key)
            .ok_or_else(|| Error::new(Span::call_site(), format!("missing `{key}` key")))
    }
}

/// Takes a string literal's value (raw or escaped; a suffix is ignored).
fn expect_str(c: &mut Cursor) -> Result<String, Error> {
    let span = c.span();
    match c.next() {
        Some(TokenTree::Literal(l)) => lit::str_lit(&l)
            .map(|s| s.value)
            .ok_or_else(|| Error::new(l.span(), "expected string literal")),
        _ => Err(Error::new(span, "expected string literal")),
    }
}

/// Runs an expansion, turning its error into a `compile_error!`.
fn emit(expansion: Result<TokenStream, Error>) -> TokenStream {
    expansion.unwrap_or_else(|e| e.to_compile_error())
}

/// Bind a fn to the model action it performs (`machine`, `action`, and `project` required).
///
/// ```compile_fail
/// #[aterm_spec_macros::refines(machine = "Ring", action = "Push")]
/// fn push() {}
/// ```
#[proc_macro_attribute]
pub fn refines(attr: TokenStream, item: TokenStream) -> TokenStream {
    emit(expand_refines(attr, item))
}

fn expand_refines(attr: TokenStream, item: TokenStream) -> Result<TokenStream, Error> {
    let keys = Keys::parse(attr, &["machine", "action", "project"])?;
    let machine = keys.require("machine")?;
    let action = keys.require("action")?;
    let project = keys
        .get("project")
        .filter(|p| !p.is_empty())
        .ok_or_else(|| {
            Error::new(
                Span::call_site(),
                "missing `project` key: name the fn that projects the real state onto the \
             model's variables",
            )
        })?;
    let item = FnItem::new(item);
    let Some(fn_ident) = item.fn_with_body().and(item.ident()) else {
        return Ok(refusal(&item));
    };
    let rust_method = fn_ident.to_string();
    // A const fn's body may not call the probe: its record says so with an empty id.
    let (entry_id, probe) = if item.fn_with_body() == Some(false) {
        let id = entry_id_for(&machine, &action, &rust_method);
        let probe = entry_probe(&id);
        (id, probe)
    } else {
        (String::new(), TokenStream::new())
    };
    // Each submit is its own anonymous const, so stacked anchors never share a name.
    let record = q!(
        "#[cfg(any(target_vendor = \"apple\", target_os = \"linux\", target_os = \"android\", windows))]
        ::aterm_spec_anchor::registry::submit! {
            ::aterm_spec_anchor::xref::RefinementAnchor => ::aterm_spec_anchor::xref::RefinementAnchor {
                machine: #machine,
                action: #action,
                rust_method: #rust_method,
                location: concat!(file!(), \":\", line!()),
                project: #project,
                entry_id: #entry_id,
            }
        }",
        machine = Literal::string(&machine),
        action = Literal::string(&action),
        rust_method = Literal::string(&rust_method),
        project = Literal::string(&project),
        entry_id = Literal::string(&entry_id)
    );
    Ok(anchored(&item, record, probe))
}

/// Waive a fn the model does not describe (`reason` required, `machine` and `action` optional).
///
/// ```
/// #[aterm_spec_macros::spec_unmodeled(reason = "a platform fast path", machine = "Twin")]
/// fn fast() -> u32 {
///     7
/// }
/// assert_eq!(fast(), 7);
/// ```
///
/// Like `refines`, it goes on a fn with a body, never on a struct or a trait's method
/// declaration:
///
/// ```compile_fail
/// #[aterm_spec_macros::spec_unmodeled(reason = "a struct")]
/// struct S;
/// ```
///
/// ```compile_fail
/// trait Decl {
///     #[aterm_spec_macros::spec_unmodeled(reason = "a declaration")]
///     fn declared(&self) -> u32;
/// }
/// ```
#[proc_macro_attribute]
pub fn spec_unmodeled(attr: TokenStream, item: TokenStream) -> TokenStream {
    emit(expand_spec_unmodeled(attr, item))
}

fn expand_spec_unmodeled(attr: TokenStream, item: TokenStream) -> Result<TokenStream, Error> {
    let keys = Keys::parse(attr, &["reason", "machine", "action"])?;
    let reason = keys.require("reason")?;
    let machine = keys.get("machine").unwrap_or_default();
    let action = keys.get("action").unwrap_or_default();
    let item = FnItem::new(item);
    let Some(fn_ident) = item.fn_with_body().and(item.ident()) else {
        return Ok(refusal(&item));
    };
    let rust_method = fn_ident.to_string();
    let record = q!(
        "#[cfg(any(target_vendor = \"apple\", target_os = \"linux\", target_os = \"android\", windows))]
        ::aterm_spec_anchor::registry::submit! {
            ::aterm_spec_anchor::xref::Waiver => ::aterm_spec_anchor::xref::Waiver {
                machine: #machine,
                action: #action,
                reason: #reason,
                rust_method: #rust_method,
                location: concat!(file!(), \":\", line!()),
            }
        }",
        machine = Literal::string(&machine),
        action = Literal::string(&action),
        reason = Literal::string(&reason),
        rust_method = Literal::string(&rust_method)
    );
    Ok(anchored(&item, record, TokenStream::new()))
}

/// Expand a bounded state-machine DSL into an `::aterm_spec::derive::Model`.
#[proc_macro]
pub fn ty_model(input: TokenStream) -> TokenStream {
    emit(model::expand(input))
}

/// The fn a registering attribute sits on, and its name: a free `fn` with a body.
fn registered_fn(item: TokenStream, attribute: &str) -> Result<(FnItem, Ident), Error> {
    let item = FnItem::new(item);
    match (item.fn_with_body(), item.ident()) {
        (Some(_), Some(ident)) => Ok((item, ident)),
        _ => Err(Error::new(
            Span::call_site(),
            format!("`#[{attribute}]` goes on a fn with a body"),
        )),
    }
}

/// Register a hand-built `fn() -> Model`.
#[proc_macro_attribute]
pub fn model(attr: TokenStream, item: TokenStream) -> TokenStream {
    emit(expand_model(attr, item))
}

fn expand_model(attr: TokenStream, item: TokenStream) -> Result<TokenStream, Error> {
    if !attr.is_empty() {
        return Err(Error::new(
            Span::call_site(),
            "`#[model]` takes no arguments",
        ));
    }
    let (item, ident) = registered_fn(item, "model")?;
    Ok(q!(
        "#f
        ::aterm_spec::registry::submit! {
            ::aterm_spec::xref::RegisteredModel => ::aterm_spec::xref::RegisteredModel {
                build: #ident,
                scenarios: &[],
                location: ::core::concat!(::core::file!(), \":\", ::core::line!()),
            }
        }",
        f = item.stream(),
        ident
    ))
}

/// Register a `fn() -> Liveness` obligation with its model.
#[proc_macro_attribute]
pub fn liveness(attr: TokenStream, item: TokenStream) -> TokenStream {
    emit(expand_liveness(attr, item))
}

fn expand_liveness(attr: TokenStream, item: TokenStream) -> Result<TokenStream, Error> {
    let over = model_path(attr)?;
    let (item, ident) = registered_fn(item, "liveness")?;
    Ok(q!(
        "#f
        ::aterm_spec::registry::submit! {
            ::aterm_spec::xref::RegisteredLiveness => ::aterm_spec::xref::RegisteredLiveness {
                model: #over,
                liveness: #ident,
                location: ::core::concat!(::core::file!(), \":\", ::core::line!()),
            }
        }",
        f = item.stream(),
        over,
        ident
    ))
}

/// The path `#[liveness(...)]` names its model's fn by: `name`, `a::b` or `::a::b`,
/// passed through as written.
fn model_path(attr: TokenStream) -> Result<TokenStream, Error> {
    let tokens: Vec<TokenTree> = Cursor::new(attr, Span::call_site()).collect();
    let mut c = Cursor::new(tokens.iter().cloned().collect(), Span::call_site());
    c.eat_op("::");
    loop {
        if !matches!(c.next(), Some(TokenTree::Ident(_))) {
            return Err(Error::spanned_by(
                &tokens,
                "expected the path of the model's `fn() -> Model`",
            ));
        }
        if c.is_empty() {
            return Ok(tokens.into_iter().collect());
        }
        if c.eat_op("::").is_none() {
            return Err(Error::new(c.span(), "expected `::` or the end of the path"));
        }
    }
}
