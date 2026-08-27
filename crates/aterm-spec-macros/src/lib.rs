// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! aterm-spec-macros: Proc macros for TLA+ specification refinement attributes.

#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::all)]

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{DeriveInput, parse_macro_input};

#[proc_macro_derive(SpecState, attributes(spec_machine))]
pub fn derive_spec_state(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    let mut machine_name = None;
    let mut tla_file = None;

    for attr in &input.attrs {
        if attr.path().is_ident("spec_machine") {
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("name") {
                    let value = meta.value()?;
                    let lit: syn::LitStr = value.parse()?;
                    machine_name = Some(lit.value());
                } else if meta.path.is_ident("tla_file") {
                    let value = meta.value()?;
                    let lit: syn::LitStr = value.parse()?;
                    tla_file = Some(lit.value());
                }
                Ok(())
            });
        }
    }

    let machine_name_str =
        machine_name.unwrap_or_else(|| name.to_string().trim_end_matches("Model").to_lowercase());
    let tla_file_str = tla_file.unwrap_or_default();

    let expanded = quote! {
        impl #name {
            pub const SPEC_MACHINE_NAME: &'static str = #machine_name_str;
            pub const SPEC_TLA_FILE: &'static str = #tla_file_str;
        }
    };

    TokenStream::from(expanded)
}

#[proc_macro_derive(SpecAction)]
pub fn derive_spec_action(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    let variant_names: Vec<String> = match &input.data {
        syn::Data::Enum(data) => data.variants.iter().map(|v| v.ident.to_string()).collect(),
        _ => {
            return syn::Error::new_spanned(name, "SpecAction can only be derived for enums")
                .to_compile_error()
                .into();
        }
    };

    let count = variant_names.len();

    let expanded = quote! {
        impl #name {
            pub const SPEC_ACTIONS: [&'static str; #count] = [
                #(#variant_names),*
            ];
        }
    };

    TokenStream::from(expanded)
}

// ---------------------------------------------------------------------------
// Source↔spec ANCHOR EMITTERS (TRUST_NATIVE_TLA, Phase 0).
//
// `#[refines]`, `#[spec_unmodeled]`, `#[spec_invariant]` were no-ops (parse the
// args, discard, return the item). They are now ANCHOR EMITTERS: the annotated
// item is left 100% unchanged at runtime, but each ALSO registers a compile-time-
// collectable record into an `inventory` distributed slice (the records live in
// `aterm_spec::xref`). The set of all bindings is then collectable at test time by
// the `spec_xref_closure` gate, with NO source-text scanning.
//
// EMISSION SHAPE — these attributes sit on `&mut self` methods INSIDE `impl`
// blocks. `inventory::submit!` expands to a module-level anonymous `const`, which
// is ILLEGAL as a bare impl item ("`const` items in this context need a name").
// So each emitter wraps the submit inside a UNIQUELY-NAMED associated `const`
// (`const __aterm_spec_<kind>_<fnident>: () = { inventory::submit!{…} };`), where
// the submit's anonymous const now lives inside a block expression (legal) and the
// associated const name is unique within its impl (fn idents are unique per impl).
// Verified to collect across crates from the host crate's unit-test build.
//
// The macro span-captures the source location via `file!()`/`line!()` and the fn
// path via the fn ident (a best-effort `rust_method` for diagnostics; the precise
// DefId binding is Phase 3's `trust-ir` job, obligation 2).
// ---------------------------------------------------------------------------

/// Extract the annotated fn's identifier (for the unique const name + the
/// `rust_method` diagnostic). Falls back to a generic name if the item is not a
/// plain fn (defensive; all current usages are methods).
fn item_fn_ident(item: &proc_macro2::TokenStream) -> proc_macro2::Ident {
    // The attribute is applied to a fn; find the `fn <ident>` token pair.
    let mut iter = item.clone().into_iter().peekable();
    while let Some(tok) = iter.next() {
        if let proc_macro2::TokenTree::Ident(id) = &tok
            && id == "fn"
            && let Some(proc_macro2::TokenTree::Ident(name)) = iter.peek()
        {
            return name.clone();
        }
    }
    format_ident!("anon")
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

// ---------------------------------------------------------------------------
// EXECUTION-EVIDENCE PROBE (the runnable half of obligation 2).
//
// `#[refines]` used to leave the annotated fn 100% unchanged, which is exactly why
// a green closure gate was a claim about STRINGS: nothing tied the anchor to a fn
// that performs the action, and an anchor moved onto `Terminal::is_tmux_mode_active()`
// (body: `false`, callers: none) kept the gate green.
//
// So the emitter now ALSO injects, as the FIRST statement of the annotated fn's
// body, `::aterm_spec::xref::note_entered("<machine>::<action> @ <fn>")`. The id is
// the same literal the submitted `RefinementAnchor` carries in `entry_id`, emitted
// from this one expansion, so the probe and the record cannot drift. A gate resets
// the thread-local record, runs the machine's Tier-1 conformance, and requires every
// anchor of that machine to have been entered.
//
// COST. Nothing changes for a shipping build: the anchors are applied through
// `#[cfg_attr(any(test, feature = "spec-anchors"), aterm_spec::refines(…))]`, so when
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

/// Prepend the entry probe to the annotated fn's body.
///
/// Returns `None` when the item is not an instrumentable fn (see the module note
/// above); the caller then emits `entry_id: ""` so the gate can see the gap instead of
/// mistaking an unprobed anchor for an entered one.
fn inject_entry_probe(
    item: &proc_macro2::TokenStream,
    entry_id: &str,
) -> Option<proc_macro2::TokenStream> {
    let probe: syn::Stmt = syn::parse_quote! {
        ::aterm_spec::xref::note_entered(#entry_id);
    };
    // `ImplItemFn` covers both the `impl`-block methods the anchors mostly sit on and
    // free fns (same shape: attrs, vis, defaultness, sig, block); `ItemFn` is the
    // belt-and-braces fallback. Inner attributes (`fn f() { #![allow(…)] … }`) are
    // parsed onto the item by syn and re-printed inside the braces, so prepending at
    // `stmts[0]` cannot displace them.
    if let Ok(mut f) = syn::parse2::<syn::ImplItemFn>(item.clone()) {
        if f.sig.constness.is_some() {
            return None;
        }
        f.block.stmts.insert(0, probe);
        return Some(quote! { #f });
    }
    if let Ok(mut f) = syn::parse2::<syn::ItemFn>(item.clone()) {
        if f.sig.constness.is_some() {
            return None;
        }
        f.block.stmts.insert(0, probe);
        return Some(quote! { #f });
    }
    None
}

#[proc_macro_attribute]
pub fn refines(attr: TokenStream, item: TokenStream) -> TokenStream {
    let meta = syn::parse_macro_input!(attr as RefinesMeta);
    let item2: proc_macro2::TokenStream = item.into();
    let fn_ident = item_fn_ident(&item2);
    // Disambiguate the anchor const by ACTION as well as fn name: a single method
    // may legitimately implement MORE THAN ONE spec action (e.g. the `write_all`
    // drain loop is both `Progress` and `Interrupted`), so two `#[refines]` on the
    // same fn must emit two DISTINCTLY-NAMED consts (a bare `__aterm_spec_refines_
    // <fn>` would collide). `sanitize_ident` keeps the suffix a legal identifier.
    let const_name = format_ident!(
        "__aterm_spec_refines_{}_{}",
        fn_ident,
        sanitize_ident(&meta.action)
    );
    let rust_method = fn_ident.to_string();
    let machine = meta.machine;
    let action = meta.action;
    let project = meta.project.unwrap_or_default();
    // Execution evidence: probe the annotated body and record the SAME id on the
    // anchor, so the gate can ask "did this fn actually run?" and not just "does this
    // string exist?". An un-probeable item keeps the old behaviour (item verbatim) and
    // says so with an empty `entry_id`.
    let candidate_id = entry_id_for(&machine, &action, &rust_method);
    let (item_out, entry_id) = match inject_entry_probe(&item2, &candidate_id) {
        Some(probed) => (probed, candidate_id),
        None => (item2, String::new()),
    };
    quote! {
        #item_out
        #[allow(non_upper_case_globals)]
        const #const_name: () = {
            ::aterm_spec::inventory::submit! {
                ::aterm_spec::xref::RefinementAnchor {
                    machine: #machine,
                    action: #action,
                    rust_method: #rust_method,
                    location: concat!(file!(), ":", line!()),
                    project: #project,
                    entry_id: #entry_id,
                }
            }
        };
    }
    .into()
}

#[proc_macro_attribute]
pub fn spec_invariant(attr: TokenStream, item: TokenStream) -> TokenStream {
    let meta = syn::parse_macro_input!(attr as InvariantMeta);
    let item2: proc_macro2::TokenStream = item.into();
    let fn_ident = item_fn_ident(&item2);
    let const_name = format_ident!("__aterm_spec_invariant_{}", fn_ident);
    let rust_method = fn_ident.to_string();
    let id = meta.id;
    let machine = meta.machine.unwrap_or_default();
    quote! {
        #item2
        #[allow(non_upper_case_globals)]
        const #const_name: () = {
            ::aterm_spec::inventory::submit! {
                ::aterm_spec::xref::InvariantAnchor {
                    machine: #machine,
                    id: #id,
                    rust_method: #rust_method,
                    location: concat!(file!(), ":", line!()),
                }
            }
        };
    }
    .into()
}

#[proc_macro_attribute]
pub fn spec_unmodeled(attr: TokenStream, item: TokenStream) -> TokenStream {
    let meta = syn::parse_macro_input!(attr as UnmodeledMeta);
    let item2: proc_macro2::TokenStream = item.into();
    let fn_ident = item_fn_ident(&item2);
    // Disambiguate by waived action (like `refines`): a fn may waive more than one
    // spec action, so two `#[spec_unmodeled]` on one fn need distinct const names.
    let const_name = format_ident!(
        "__aterm_spec_unmodeled_{}_{}",
        fn_ident,
        sanitize_ident(meta.action.as_deref().unwrap_or(""))
    );
    let rust_method = fn_ident.to_string();
    let reason = meta.reason;
    let machine = meta.machine.unwrap_or_default();
    let action = meta.action.unwrap_or_default();
    quote! {
        #item2
        #[allow(non_upper_case_globals)]
        const #const_name: () = {
            ::aterm_spec::inventory::submit! {
                ::aterm_spec::xref::Waiver {
                    machine: #machine,
                    action: #action,
                    reason: #reason,
                    rust_method: #rust_method,
                    location: concat!(file!(), ":", line!()),
                }
            }
        };
    }
    .into()
}

struct RefinesMeta {
    machine: String,
    action: String,
    project: Option<String>,
}

impl syn::parse::Parse for RefinesMeta {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let mut machine = None;
        let mut action = None;
        let mut project = None;
        while !input.is_empty() {
            let ident: syn::Ident = input.parse()?;
            let _: syn::Token![=] = input.parse()?;
            let lit: syn::LitStr = input.parse()?;
            match ident.to_string().as_str() {
                "machine" => machine = Some(lit.value()),
                "action" => action = Some(lit.value()),
                // The projection fn path (`project="…"`): names the &self -> abstract
                // vars mapping used by Tier-1 conformance. Optional in Phase 0.
                "project" => project = Some(lit.value()),
                other => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("unknown key: {other}"),
                    ));
                }
            }
            if !input.is_empty() {
                let _: syn::Token![,] = input.parse()?;
            }
        }
        Ok(RefinesMeta {
            machine: machine
                .ok_or_else(|| syn::Error::new(input.span(), "missing `machine` key"))?,
            action: action.ok_or_else(|| syn::Error::new(input.span(), "missing `action` key"))?,
            project,
        })
    }
}

struct InvariantMeta {
    id: String,
    machine: Option<String>,
    _tla: Option<String>,
}

impl syn::parse::Parse for InvariantMeta {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let mut id = None;
        let mut machine = None;
        let mut tla = None;
        while !input.is_empty() {
            let ident: syn::Ident = input.parse()?;
            let _: syn::Token![=] = input.parse()?;
            let lit: syn::LitStr = input.parse()?;
            match ident.to_string().as_str() {
                "id" => id = Some(lit.value()),
                "machine" => machine = Some(lit.value()),
                "tla" => tla = Some(lit.value()),
                other => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("unknown key: {other}"),
                    ));
                }
            }
            if !input.is_empty() {
                let _: syn::Token![,] = input.parse()?;
            }
        }
        Ok(InvariantMeta {
            id: id.ok_or_else(|| syn::Error::new(input.span(), "missing `id` key"))?,
            machine,
            _tla: tla,
        })
    }
}

struct UnmodeledMeta {
    reason: String,
    machine: Option<String>,
    action: Option<String>,
}

impl syn::parse::Parse for UnmodeledMeta {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let mut reason = None;
        let mut machine = None;
        let mut action = None;
        while !input.is_empty() {
            let ident: syn::Ident = input.parse()?;
            let _: syn::Token![=] = input.parse()?;
            let lit: syn::LitStr = input.parse()?;
            match ident.to_string().as_str() {
                "reason" => reason = Some(lit.value()),
                // Optional: tie the waiver to a specific machine action so it
                // discharges that action for the coverage obligation (§2.2 (3)).
                "machine" => machine = Some(lit.value()),
                "action" => action = Some(lit.value()),
                other => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("unknown key: {other}"),
                    ));
                }
            }
            if !input.is_empty() {
                let _: syn::Token![,] = input.parse()?;
            }
        }
        Ok(UnmodeledMeta {
            reason: reason.ok_or_else(|| syn::Error::new(input.span(), "missing `reason` key"))?,
            machine,
            action,
        })
    }
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
//       }
//   }
//
// Identifiers declared `const` become TLA+ CONSTANTS; everything else resolves to
// a state variable. Supported operators: + - > <= == && and if/else (mapped to
// the `Expr` builders in `aterm_spec::derive`: `==` -> `eq` = TLA+ `=`, `&&` ->
// `and_` = TLA+ `/\`). The `==`/`&&` forms support the `Buggy`-convention guards
// (e.g. `Buggy == 1 && active == 1`) the proves-and-catches models use. The macro
// emits absolute `::aterm_spec::derive::*` paths, so it works in any crate
// depending on aterm-spec (and in aterm-spec's own integration tests).
// ---------------------------------------------------------------------------

/// See module-level note above. Expands to an `::aterm_spec::derive::Model`.
#[proc_macro]
pub fn ty_model(input: TokenStream) -> TokenStream {
    let def = parse_macro_input!(input as ModelDef);
    match def.expand() {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

struct ActionDef {
    name: syn::Ident,
    guard: Option<syn::Expr>,
    updates: Vec<(syn::Ident, syn::Expr)>,
}

struct ModelDef {
    name: syn::Ident,
    consts: Vec<(syn::Ident, i64)>,
    vars: Vec<(syn::Ident, i64)>,
    actions: Vec<ActionDef>,
    invariants: Vec<(syn::Ident, syn::Expr)>,
}

impl syn::parse::Parse for ModelDef {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let name: syn::Ident = input.parse()?;
        let body;
        syn::braced!(body in input);
        let mut consts = Vec::new();
        let mut vars = Vec::new();
        let mut actions = Vec::new();
        let mut invariants = Vec::new();
        while !body.is_empty() {
            if body.peek(syn::Token![const]) {
                body.parse::<syn::Token![const]>()?;
                let id: syn::Ident = body.parse()?;
                body.parse::<syn::Token![=]>()?;
                let lit: syn::LitInt = body.parse()?;
                body.parse::<syn::Token![;]>()?;
                consts.push((id, lit.base10_parse::<i64>()?));
                continue;
            }
            let kw: syn::Ident = body.parse()?;
            match kw.to_string().as_str() {
                "var" => {
                    let id: syn::Ident = body.parse()?;
                    body.parse::<syn::Token![=]>()?;
                    let lit: syn::LitInt = body.parse()?;
                    body.parse::<syn::Token![;]>()?;
                    vars.push((id, lit.base10_parse::<i64>()?));
                }
                "action" => {
                    let aname: syn::Ident = body.parse()?;
                    let guard = if body.peek(syn::Ident) {
                        let w: syn::Ident = body.parse()?;
                        if w != "when" {
                            return Err(syn::Error::new(w.span(), "expected `when` or `{`"));
                        }
                        let g;
                        syn::parenthesized!(g in body);
                        Some(g.parse::<syn::Expr>()?)
                    } else {
                        None
                    };
                    let ab;
                    syn::braced!(ab in body);
                    let mut updates = Vec::new();
                    while !ab.is_empty() {
                        let lhs: syn::Ident = ab.parse()?;
                        ab.parse::<syn::Token![=]>()?;
                        let rhs: syn::Expr = ab.parse()?;
                        ab.parse::<syn::Token![;]>()?;
                        updates.push((lhs, rhs));
                    }
                    actions.push(ActionDef {
                        name: aname,
                        guard,
                        updates,
                    });
                }
                "invariant" => {
                    let iname: syn::Ident = body.parse()?;
                    body.parse::<syn::Token![:]>()?;
                    let e: syn::Expr = body.parse()?;
                    body.parse::<syn::Token![;]>()?;
                    invariants.push((iname, e));
                }
                other => {
                    return Err(syn::Error::new(
                        kw.span(),
                        format!("expected const/var/action/invariant, found `{other}`"),
                    ));
                }
            }
        }
        Ok(ModelDef {
            name,
            consts,
            vars,
            actions,
            invariants,
        })
    }
}

impl ModelDef {
    fn expand(&self) -> syn::Result<proc_macro2::TokenStream> {
        let const_names: std::collections::HashSet<String> =
            self.consts.iter().map(|(id, _)| id.to_string()).collect();
        let name_str = self.name.to_string();

        let consts_toks = self.consts.iter().map(|(id, v)| {
            let s = id.to_string();
            quote! { (#s, #v) }
        });
        let vars_toks = self.vars.iter().map(|(id, v)| {
            let s = id.to_string();
            quote! { ::aterm_spec::derive::StateVar { name: #s, init: #v } }
        });

        let mut actions_toks = Vec::new();
        for a in &self.actions {
            let an = a.name.to_string();
            let guard = match &a.guard {
                Some(g) => {
                    let t = tr_expr(g, &const_names)?;
                    quote! { Some(#t) }
                }
                None => quote! { None },
            };
            let mut ups = Vec::new();
            for (lhs, rhs) in &a.updates {
                let s = lhs.to_string();
                let t = tr_expr(rhs, &const_names)?;
                ups.push(quote! { ::aterm_spec::derive::Update { var: #s, expr: #t } });
            }
            actions_toks.push(quote! {
                ::aterm_spec::derive::Action { name: #an, guard: #guard, updates: vec![ #(#ups),* ] }
            });
        }

        let mut invs_toks = Vec::new();
        for (id, e) in &self.invariants {
            let s = id.to_string();
            let t = tr_expr(e, &const_names)?;
            invs_toks.push(quote! { ::aterm_spec::derive::Invariant { name: #s, expr: #t } });
        }

        Ok(quote! {
            ::aterm_spec::derive::Model {
                name: #name_str,
                consts: vec![ #(#consts_toks),* ],
                vars: vec![ #(#vars_toks),* ],
                fn_vars: vec![],
                actions: vec![ #(#actions_toks),* ],
                invariants: vec![ #(#invs_toks),* ],
            }
        })
    }
}

/// The single tail expression of a one-expression block (`{ expr }`).
fn block_tail(block: &syn::Block) -> syn::Result<&syn::Expr> {
    if let [syn::Stmt::Expr(e, None)] = block.stmts.as_slice() {
        Ok(e)
    } else {
        Err(syn::Error::new_spanned(
            block,
            "if/else branch must be a single expression",
        ))
    }
}

/// Translate the else branch (a `{ block }` or a nested `if`) to an `Expr`.
fn tr_else(
    e: &syn::Expr,
    consts: &std::collections::HashSet<String>,
) -> syn::Result<proc_macro2::TokenStream> {
    match e {
        syn::Expr::Block(b) => tr_expr(block_tail(&b.block)?, consts),
        _ => tr_expr(e, consts),
    }
}

/// Translate a restricted `syn::Expr` to `aterm_spec::derive::Expr` builder calls.
fn tr_expr(
    e: &syn::Expr,
    consts: &std::collections::HashSet<String>,
) -> syn::Result<proc_macro2::TokenStream> {
    match e {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(i),
            ..
        }) => {
            let v: i64 = i.base10_parse()?;
            Ok(quote! { ::aterm_spec::derive::int(#v) })
        }
        syn::Expr::Path(p) => {
            if let Some(id) = p.path.get_ident() {
                let id = id.to_string();
                if consts.contains(&id) {
                    Ok(quote! { ::aterm_spec::derive::cst(#id) })
                } else {
                    Ok(quote! { ::aterm_spec::derive::var(#id) })
                }
            } else {
                Err(syn::Error::new_spanned(e, "expected a simple identifier"))
            }
        }
        syn::Expr::Paren(p) => tr_expr(&p.expr, consts),
        syn::Expr::Binary(b) => {
            let l = tr_expr(&b.left, consts)?;
            let r = tr_expr(&b.right, consts)?;
            let f = match b.op {
                syn::BinOp::Add(_) => quote! { ::aterm_spec::derive::add },
                syn::BinOp::Sub(_) => quote! { ::aterm_spec::derive::sub },
                syn::BinOp::Gt(_) => quote! { ::aterm_spec::derive::gt },
                syn::BinOp::Le(_) => quote! { ::aterm_spec::derive::le },
                syn::BinOp::Eq(_) => quote! { ::aterm_spec::derive::eq },
                syn::BinOp::And(_) => quote! { ::aterm_spec::derive::and_ },
                syn::BinOp::Or(_) => quote! { ::aterm_spec::derive::or_ },
                _ => {
                    return Err(syn::Error::new_spanned(
                        e,
                        "unsupported operator (use + - > <= == && ||)",
                    ));
                }
            };
            Ok(quote! { #f(#l, #r) })
        }
        syn::Expr::If(ifx) => {
            let Some((_, else_expr)) = &ifx.else_branch else {
                return Err(syn::Error::new_spanned(
                    e,
                    "`if` must have an `else` branch",
                ));
            };
            let c = tr_expr(&ifx.cond, consts)?;
            let t = tr_expr(block_tail(&ifx.then_branch)?, consts)?;
            let f = tr_else(else_expr, consts)?;
            Ok(quote! { ::aterm_spec::derive::if_(#c, #t, #f) })
        }
        other => Err(syn::Error::new_spanned(
            other,
            "unsupported expression in ty_model!",
        )),
    }
}
