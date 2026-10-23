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

/// The name the anchor's const and `rust_method` take from: the item's `fn` ident, or
/// `anon` when the item is not a plain fn (defensive; all current usages are methods).
fn item_fn_ident(item: &FnItem) -> Ident {
    item.ident()
        .unwrap_or_else(|| Ident::new("anon", Span::call_site()))
}

/// Map an arbitrary spec name (action/id) to a legal Rust identifier fragment for
/// use in a unique const name: non-alphanumeric chars become `_`.
fn sanitize_ident(s: &str) -> String {
    if s.is_empty() {
        return "none".to_string();
    }
    s.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect()
}

/// The anchor's const, `__aterm_spec_<kind>_<fn>[_<suffix>]`, at the fn ident's span.
fn anchor_const(kind: &str, suffix: Option<&str>, fn_ident: &Ident) -> Result<Ident, Error> {
    let fn_name = fn_ident.to_string();
    let fn_name = fn_name.strip_prefix("r#").unwrap_or(&fn_name);
    let name = match suffix {
        Some(suffix) => format!("__aterm_spec_{kind}_{fn_name}_{suffix}"),
        None => format!("__aterm_spec_{kind}_{fn_name}"),
    };
    let lexed: Vec<TokenTree> = name
        .parse::<TokenStream>()
        .map(|s| s.into_iter().collect())
        .unwrap_or_default();
    match lexed.as_slice() {
        [TokenTree::Ident(i)] if i.to_string() == name => Ok(Ident::new(&name, fn_ident.span())),
        _ => Err(Error::new(
            fn_ident.span(),
            format!("`{name}` is not an identifier: name the action with letters, digits and `_`"),
        )),
    }
}

/// The execution-evidence key for one anchor: `"<machine>::<action> @ <rust_method>"`.
fn entry_id_for(machine: &str, action: &str, rust_method: &str) -> String {
    format!("{machine}::{action} @ {rust_method}")
}

/// The annotated fn with the entry probe as the first statement of its body.
fn inject_entry_probe(item: &FnItem, entry_id: &str) -> Option<TokenStream> {
    let probe = q!(
        "::aterm_spec_anchor::xref::note_entered(#id);",
        id = Literal::string(entry_id)
    );
    item.with_probe(&probe)
}

/// The `key = "value"` list an anchor attribute takes, each key one of `keys`.
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
    let fn_ident = item_fn_ident(&item);
    let const_name = anchor_const("refines", Some(&sanitize_ident(&action)), &fn_ident)?;
    let rust_method = fn_ident.to_string();
    let candidate_id = entry_id_for(&machine, &action, &rust_method);
    let (item_out, entry_id) = match inject_entry_probe(&item, &candidate_id) {
        Some(probed) => (probed, candidate_id),
        None => (item.stream(), String::new()),
    };
    Ok(q!(
        "#item_out
        #[cfg(any(target_vendor = \"apple\", target_os = \"linux\", target_os = \"android\", windows))]
        #[allow(non_upper_case_globals)]
        const #const_name: () = {
            ::aterm_spec_anchor::registry::submit! {
                ::aterm_spec_anchor::xref::RefinementAnchor => ::aterm_spec_anchor::xref::RefinementAnchor {
                    machine: #machine,
                    action: #action,
                    rust_method: #rust_method,
                    location: concat!(file!(), \":\", line!()),
                    project: #project,
                    entry_id: #entry_id,
                }
            }
        };",
        item_out,
        const_name,
        machine = Literal::string(&machine),
        action = Literal::string(&action),
        rust_method = Literal::string(&rust_method),
        project = Literal::string(&project),
        entry_id = Literal::string(&entry_id)
    ))
}

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
    let fn_ident = item_fn_ident(&item);
    let const_name = anchor_const("unmodeled", Some(&sanitize_ident(&action)), &fn_ident)?;
    let rust_method = fn_ident.to_string();
    Ok(q!(
        "#item
        #[cfg(any(target_vendor = \"apple\", target_os = \"linux\", target_os = \"android\", windows))]
        #[allow(non_upper_case_globals)]
        const #const_name: () = {
            ::aterm_spec_anchor::registry::submit! {
                ::aterm_spec_anchor::xref::Waiver => ::aterm_spec_anchor::xref::Waiver {
                    machine: #machine,
                    action: #action,
                    reason: #reason,
                    rust_method: #rust_method,
                    location: concat!(file!(), \":\", line!()),
                }
            }
        };",
        item = item.stream(),
        const_name,
        machine = Literal::string(&machine),
        action = Literal::string(&action),
        reason = Literal::string(&reason),
        rust_method = Literal::string(&rust_method)
    ))
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
