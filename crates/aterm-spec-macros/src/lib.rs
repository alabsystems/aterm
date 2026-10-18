// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! aterm-spec-macros: Proc macros for TLA+ specification refinement attributes.
//!
//! The input is read with `aterm-macro-support`, on std `proc_macro` alone: no syn,
//! proc-macro2 or quote (docs/AUDIT-third-party-2026-10-15.md, SP.8).

mod item;
mod model;
mod refusals;

use aterm_macro_support::{Cursor, Error, lit, q};
use proc_macro::{Ident, Literal, Span, TokenStream, TokenTree};

use crate::item::FnItem;

// ---------------------------------------------------------------------------
// Source↔spec ANCHOR EMITTERS (TRUST_NATIVE_TLA, Phase 0).
//
// `#[refines]` and `#[spec_unmodeled]` were no-ops (parse the
// args, discard, return the item). They are now ANCHOR EMITTERS: the annotated
// item is left 100% unchanged at runtime, but each ALSO registers a compile-time-
// collectable record into an `aterm_spec_anchor::registry` list (the records live in
// `aterm_spec_anchor::xref`; the anchored crate depends on that std-only crate, never on
// the models). The set of all bindings is then collectable at test time by
// the `spec_xref_closure` gate, with NO source-text scanning.
//
// EMISSION SHAPE — these attributes sit on `&mut self` methods INSIDE `impl`
// blocks. `registry::submit!` expands to a module-level anonymous `const`, which
// is ILLEGAL as a bare impl item ("`const` items in this context need a name").
// So each emitter wraps the submit inside a UNIQUELY-NAMED associated `const`
// (`const __aterm_spec_<kind>_<fnident>: () = { registry::submit!{…} };`), where
// the submit's anonymous const now lives inside a block expression (legal) and the
// associated const name is unique within its impl (fn idents are unique per impl).
// Verified to collect across crates from the host crate's unit-test build.
//
// TARGETS — the record is emitted only where `registry::submit!` has a load-time
// constructor section (Apple, Linux, Android, Windows: the list its compile_error
// names). Elsewhere, wasm32 in practice, nothing could collect it, so the emitters
// leave the record out and a dev build for that target still compiles; the
// `refines` entry probe stays, since a thread-local works there.
//
// The macro span-captures the source location via `file!()`/`line!()` and the fn
// path via the fn ident (a best-effort `rust_method` for diagnostics; the precise
// DefId binding is Phase 3's `trust-ir` job, obligation 2).
// ---------------------------------------------------------------------------

/// The name the anchor's const and `rust_method` take from: the item's `fn` ident, or
/// `anon` when the item is not a plain fn (defensive; all current usages are methods).
fn item_fn_ident(item: &FnItem) -> Ident {
    item.ident()
        .unwrap_or_else(|| Ident::new("anon", Span::call_site()))
}

/// Map an arbitrary spec name (action/id) to a legal Rust identifier fragment for
/// use in a unique const name: non-alphanumeric chars become `_`. Empty maps to
/// `none` so a bare (machine-less) waiver still produces a valid suffix.
fn sanitize_ident(s: &str) -> String {
    if s.is_empty() {
        return "none".to_string();
    }
    s.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect()
}

/// The anchor's const, `__aterm_spec_<kind>_<fn>[_<suffix>]`, at the fn ident's span.
///
/// A raw `r#name` contributes `name`. A name that still is not an identifier (an
/// alphanumeric character Rust does not take in one) is an error, not a panic.
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

// ---------------------------------------------------------------------------
// EXECUTION-EVIDENCE PROBE (the runnable half of obligation 2).
//
// `#[refines]` used to leave the annotated fn 100% unchanged, which is exactly why
// a green closure gate was a claim about STRINGS: nothing tied the anchor to a fn
// that performs the action, and an anchor moved onto `Terminal::is_tmux_mode_active()`
// (body: `false`, callers: none; since deleted) kept the gate green.
//
// So the emitter now ALSO injects, as the FIRST statement of the annotated fn's
// body, `::aterm_spec_anchor::xref::note_entered("<machine>::<action> @ <fn>")`. The id is
// the same literal the submitted `RefinementAnchor` carries in `entry_id`, emitted
// from this one expansion, so the probe and the record cannot drift. A gate resets
// the thread-local record, runs the machine's Tier-1 conformance, and requires every
// anchor of that machine to have been entered.
//
// COST. Nothing changes for a release build: the anchors are applied through
// `#[cfg_attr(any(test, debug_assertions), aterm_spec_anchor::refines(…))]`, so when
// that cfg is off the ATTRIBUTE ITSELF is stripped, this macro never runs, and no probe
// exists to optimize away.
//
// BEHAVIOUR. The injection is a statement prepended inside the existing block, so the
// fn's signature, generics, receiver (including `self` by value), return type and
// tail expression are untouched; an early `return`, a `?`, a recursive call and a
// diverging body all still do exactly what they did. A fn carrying SEVERAL anchors
// gets several probes: attribute macros expand outside-in, so each expansion prepends
// its own statement to the body the next one will see.
//
// NOT INSTRUMENTABLE. Two shapes get no probe and are reported as such (`entry_id: ""`):
//   * a `const fn` — its body may not call a non-const fn, and adding one would be a
//     compile error rather than a check (aterm-gpu's `video_slot_transition` and friends
//     are const);
//   * anything that is not a fn with a block (e.g. a bodyless trait method).
// `entry_id: ""` is NOT a pass — a gate demanding execution evidence must fail on it,
// or parking an anchor on a `const fn` would be a way to opt out of the check.
// ---------------------------------------------------------------------------

/// The execution-evidence key for one anchor: `"<machine>::<action> @ <rust_method>"`.
/// Human-readable because it is what a failing gate prints.
fn entry_id_for(machine: &str, action: &str, rust_method: &str) -> String {
    format!("{machine}::{action} @ {rust_method}")
}

/// The annotated fn with the entry probe as the first statement of its body.
///
/// Returns `None` when the item is not an instrumentable fn (see the module note
/// above); the caller then emits `entry_id: ""` so the gate can see the gap instead of
/// mistaking an unprobed anchor for an entered one. Inner attributes
/// (`fn f() { #![allow(…)] … }`) stay first in the body, so the probe cannot displace
/// them.
fn inject_entry_probe(item: &FnItem, entry_id: &str) -> Option<TokenStream> {
    let probe = q!(
        "::aterm_spec_anchor::xref::note_entered(#id);",
        id = Literal::string(entry_id)
    );
    item.with_probe(&probe)
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

/// Bind a fn to the model action it performs: `machine`, `action` and the
/// `project` fn Tier-1 reads the real state through, all required.
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
    // The projection fn path (`project="…"`): names the &self -> abstract vars mapping
    // Tier-1 conformance reads the real state through. Required and non-empty: an anchor
    // with no projection binds a fn to an action with nothing to compare the real state
    // against.
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
    // Disambiguate the anchor const by ACTION as well as fn name: a single method
    // may legitimately implement MORE THAN ONE spec action (e.g. the `write_all`
    // drain loop is both `Progress` and `Interrupted`), so two `#[refines]` on the
    // same fn must emit two DISTINCTLY-NAMED consts (a bare `__aterm_spec_refines_
    // <fn>` would collide). `sanitize_ident` keeps the suffix a legal identifier.
    let const_name = anchor_const("refines", Some(&sanitize_ident(&action)), &fn_ident)?;
    let rust_method = fn_ident.to_string();
    // Execution evidence: probe the annotated body and record the SAME id on the
    // anchor, so the gate can ask "did this fn actually run?" and not just "does this
    // string exist?". An un-probeable item keeps the old behaviour (item verbatim) and
    // says so with an empty `entry_id`.
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
    // `machine` and `action` are optional: they tie the waiver to a specific machine
    // action so it discharges that action for the coverage obligation (§2.2 (3)).
    let keys = Keys::parse(attr, &["reason", "machine", "action"])?;
    let reason = keys.require("reason")?;
    let machine = keys.get("machine").unwrap_or_default();
    let action = keys.get("action").unwrap_or_default();
    let item = FnItem::new(item);
    let fn_ident = item_fn_ident(&item);
    // Disambiguate by waived action (like `refines`): a fn may waive more than one
    // spec action, so two `#[spec_unmodeled]` on one fn need distinct const names.
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

// ---------------------------------------------------------------------------
// ty_model! — the "light annotation" surface for aterm-spec's derived TLA+.
//
// Write a bounded state machine as near-plain Rust; expands to an
// `::aterm_spec::derive::Model` literal (the single source from which both the
// `ty`-checkable TLA+ spec and the executable interpreter are generated). E.g.:
//
//   ty_model! {
//       Ring {
//           const MaxSeq = 6;
//           const Cap = 3;
//           var seq = 0;
//           var lo = 1;
//           action Push when (seq <= MaxSeq - 1) {
//               seq = seq + 1;
//               lo = if (seq + 1) - lo + 1 > Cap { lo + 1 } else { lo };
//           }
//           invariant LenBounded: seq - lo + 1 <= Cap;
//           bound SeqBounded: seq <= MaxSeq;
//       }
//   }
//
// Identifiers declared `const` become TLA+ CONSTANTS; everything else resolves to
// a state variable. `scenario Budget = 2;` declares a configuration the real code
// also runs under (constant overrides, never `Buggy`): the sweeps hold the model
// in it beside the committed one. `invariant` states a design claim, which some
// mutant must break; `bound` states the box the checkers walk (a range over one
// variable), which none is expected to. Supported operators: + - > <= == && || and if/else (mapped to the `Expr`
// builders in `aterm_spec::derive`: `==` -> `eq` = TLA+ `=`, `&&` ->
// `and_` = TLA+ `/\`). The `==`/`&&` forms support the `Buggy`-convention guards
// (e.g. `Buggy == 1 && active == 1`) the proves-and-catches models use. The macro
// emits absolute `::aterm_spec::derive::*` paths, so it works in any crate
// depending on aterm-spec (and in aterm-spec's own integration tests).
// ---------------------------------------------------------------------------

/// See module-level note above. Expands to an `::aterm_spec::derive::Model`.
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

/// Register a hand-built model: `#[aterm_spec::model]` on a `fn() -> Model`.
///
/// `ty_model!` registers what it writes; this is the door for a model built as
/// a `Model` literal or by a `derive::props` generator (function-valued state,
/// shapes the macro cannot spell).
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

/// Register a liveness obligation with the model it is stated over:
/// `#[aterm_spec::liveness(some_model)]` on a `fn() -> Liveness`, where
/// `some_model` is the `fn() -> Model` it holds of.
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
