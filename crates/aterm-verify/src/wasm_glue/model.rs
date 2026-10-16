// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The module's JS interface: the descriptor's records, checked against
//! each other and against the module, and arranged as classes and
//! functions. Every check fails closed with what was found and where.

use std::collections::{BTreeMap, BTreeSet};

use aterm_wasm_abi::descriptor::{self as d, Record, Ty};

use super::wasm::{FuncType, Kind, Module, code};

/// One callable: a free function or a class member.
#[derive(Clone, Debug)]
pub(crate) struct Func {
    pub(crate) kind: u8,
    pub(crate) js_name: String,
    pub(crate) symbol: String,
    pub(crate) docs: String,
    pub(crate) receiver: bool,
    pub(crate) params: Vec<(String, Ty)>,
    pub(crate) ret: Ty,
}

#[derive(Clone, Debug)]
pub(crate) struct Class {
    pub(crate) name: String,
    pub(crate) docs: String,
    pub(crate) free: String,
    pub(crate) constructor: Option<Func>,
    /// Methods, statics and getters, by JS name.
    pub(crate) members: Vec<Func>,
}

/// A JS function the module imports, one the glue knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Host {
    ConsoleError,
    ErrorStack,
    PerformanceNow,
    DateNow,
}

impl Host {
    /// The import's module and name, and its signature.
    pub(crate) fn spec(self) -> (&'static str, &'static str, &'static [u8], &'static [u8]) {
        const I32: u8 = 0x7f;
        const F64: u8 = 0x7c;
        match self {
            Host::ConsoleError => ("aterm_host", "console_error", &[I32, I32], &[]),
            Host::ErrorStack => ("aterm_host", "error_stack", &[I32], &[]),
            Host::PerformanceNow => ("aterm_host", "performance_now", &[], &[F64]),
            Host::DateNow => ("aterm_host", "date_now", &[], &[F64]),
        }
    }

    const ALL: [Host; 4] = [
        Host::ConsoleError,
        Host::ErrorStack,
        Host::PerformanceNow,
        Host::DateNow,
    ];
}

/// Everything the glue is written from.
#[derive(Clone, Debug)]
pub(crate) struct Interface {
    pub(crate) classes: Vec<Class>,
    pub(crate) functions: Vec<Func>,
    /// The imports, in the module's order.
    pub(crate) imports: Vec<Host>,
    /// Every exported function, by name, with its signature: the raw
    /// interface `_bg.wasm.d.ts` and `InitOutput` describe.
    pub(crate) raw: BTreeMap<String, FuncType>,
}

/// The prefix of the exports no record describes: the runtime's own below,
/// and link-time seams other crates define for the module (aterm-time's
/// `aterm_wasm_date_now`). Every other function export needs a record.
pub(crate) const UNRECORDED_PREFIX: &str = "aterm_";

/// The runtime's own exports the glue calls.
pub(crate) const ALLOC: &str = "aterm_alloc";
pub(crate) const REALLOC: &str = "aterm_realloc";
pub(crate) const FREE: &str = "aterm_free";
pub(crate) const RET_SLOT: &str = "aterm_ret_slot";

/// Exports the generator drops from the module it writes: the linker's
/// layout globals, which no JS reads (wasm-bindgen drops them too).
pub(crate) const DROPPED_EXPORTS: [&str; 2] = ["__data_end", "__heap_base"];

/// Read and check the module's interface.
pub(crate) fn interface(m: &Module<'_>) -> Result<Interface, String> {
    let sections = m.custom(d::SECTION);
    if sections.is_empty() {
        return Err(format!(
            "the module has no `{}` custom section: it was not built with aterm-wasm-abi's \
             #[export], or a strip or wasm-opt pass ran before wasm-glue (bind the module \
             cargo wrote)",
            d::SECTION
        ));
    }
    let mut records = Vec::new();
    for s in sections {
        records.extend(d::decode(s).map_err(|e| e.to_string())?);
    }
    for record in &records {
        identifier(&record.js_name)?;
        identifier(&record.symbol)?;
        if !record.class.is_empty() {
            identifier(&record.class)?;
            // Classes are unrenamed Rust struct identifiers. `$` belongs to
            // the generator's private locals, which must never shadow a
            // class used to lift a returned handle.
            if record.class.contains('$') {
                return Err(format!(
                    "class `{}` is not a Rust struct identifier",
                    record.class
                ));
            }
        }
        for (name, _) in &record.params {
            identifier(name)?;
        }
    }
    if records.is_empty() {
        return Err(format!("the `{}` section holds no records", d::SECTION));
    }

    let exports: BTreeMap<&str, &super::wasm::Export> =
        m.exports.iter().map(|e| (e.name.as_str(), e)).collect();
    let sig_of = |symbol: &str| -> Result<&FuncType, String> {
        let e = exports.get(symbol).ok_or_else(|| {
            format!("the descriptor names export `{symbol}`, which the module lacks")
        })?;
        if e.kind != Kind::Func {
            return Err(format!("export `{symbol}` is not a function"));
        }
        m.func_type(e.index)
            .ok_or_else(|| format!("export `{symbol}` names no function"))
    };

    let mut classes: BTreeMap<String, Class> = BTreeMap::new();
    for r in records.iter().filter(|r| r.kind == d::kind::CLASS) {
        if r.class != r.js_name || r.class.is_empty() {
            return Err(format!("class record `{}` names `{}`", r.class, r.js_name));
        }
        top_level_name(&r.class)?;
        let expect = FuncType {
            params: vec![0x7f, 0x7f],
            results: vec![],
        };
        check_sig(&r.symbol, sig_of(&r.symbol)?, &expect)?;
        let class = Class {
            name: r.class.clone(),
            docs: r.docs.clone(),
            free: r.symbol.clone(),
            constructor: None,
            members: Vec::new(),
        };
        if classes.insert(r.class.clone(), class).is_some() {
            return Err(format!("two classes are named `{}`", r.class));
        }
    }

    let mut functions = Vec::new();
    let mut free_names = BTreeSet::new();
    for r in records.iter().filter(|r| r.kind != d::kind::CLASS) {
        let f = func(r, &classes)?;
        check_sig(&r.symbol, sig_of(&r.symbol)?, &lowered(&f)?)?;
        if r.kind == d::kind::FUNCTION {
            top_level_name(&f.js_name)?;
            if !free_names.insert(f.js_name.clone()) || classes.contains_key(&f.js_name) {
                return Err(format!("two exports are named `{}`", f.js_name));
            }
            functions.push(f);
            continue;
        }
        let class = classes.get_mut(&r.class).ok_or_else(|| {
            format!(
                "`{}` belongs to `{}`, which no #[export] struct declares",
                r.js_name, r.class
            )
        })?;
        if r.kind == d::kind::CONSTRUCTOR {
            if class.constructor.is_some() {
                return Err(format!("class `{}` has two constructors", r.class));
            }
            class.constructor = Some(f);
        } else {
            if class.members.iter().any(|m| m.js_name == f.js_name)
                || [
                    "free",
                    "constructor",
                    "__wrap",
                    "__destroy_into_raw",
                    "__wbg_ptr",
                ]
                .contains(&f.js_name.as_str())
                || (f.kind == d::kind::STATIC && f.js_name == "prototype")
            {
                return Err(format!(
                    "class `{}` has two members named `{}`, or one with a name the glue \
                     reserves",
                    r.class, f.js_name
                ));
            }
            class.members.push(f);
        }
    }
    functions.sort_by(|a, b| a.js_name.cmp(&b.js_name));
    for c in classes.values_mut() {
        c.members.sort_by(|a, b| a.js_name.cmp(&b.js_name));
    }

    let mut imports = Vec::new();
    for i in &m.imports {
        let host = Host::ALL
            .into_iter()
            .find(|h| {
                let (module, name, _, _) = h.spec();
                module == i.module && name == i.name
            })
            .ok_or_else(|| {
                format!(
                    "the module imports `{}.{}`, which the glue does not provide (aterm-wasm-abi's \
                     host.rs lists what it does)",
                    i.module, i.name
                )
            })?;
        let (_, _, params, results) = host.spec();
        let got = (i.kind == Kind::Func)
            .then(|| i.ty.and_then(|t| m.types.get(t as usize)))
            .flatten();
        if got
            != Some(&FuncType {
                params: params.to_vec(),
                results: results.to_vec(),
            })
        {
            return Err(format!(
                "import `{}.{}` has another signature than the glue's",
                i.module, i.name
            ));
        }
        imports.push(host);
    }

    let recorded: BTreeSet<&str> = records.iter().map(|r| r.symbol.as_str()).collect();
    let mut raw = BTreeMap::new();
    for e in &m.exports {
        if DROPPED_EXPORTS.contains(&e.name.as_str()) {
            continue;
        }
        match e.kind {
            Kind::Func => {
                identifier(&e.name)?;
                if RESERVED.contains(&e.name.as_str()) {
                    return Err(format!("export `{}` is a JS reserved word", e.name));
                }
                if !recorded.contains(e.name.as_str()) && !e.name.starts_with(UNRECORDED_PREFIX) {
                    return Err(format!(
                        "export `{}` has no descriptor record: the glue would drop it \
                         silently (export it with #[export], or name a link-time seam \
                         `{UNRECORDED_PREFIX}*`)",
                        e.name
                    ));
                }
                let ty = m
                    .func_type(e.index)
                    .ok_or_else(|| format!("export `{}` names no function", e.name))?;
                raw.insert(e.name.clone(), ty.clone());
            }
            Kind::Memory if e.name == "memory" => {}
            _ => {
                return Err(format!(
                    "export `{}` is a {:?}; the glue describes functions and `memory` only",
                    e.name, e.kind
                ));
            }
        }
    }
    if !m
        .exports
        .iter()
        .any(|e| e.name == "memory" && e.kind == Kind::Memory)
    {
        return Err("the module exports no `memory`".to_string());
    }
    let runtime = [
        (
            ALLOC,
            FuncType {
                params: vec![0x7f; 2],
                results: vec![0x7f],
            },
        ),
        (
            REALLOC,
            FuncType {
                params: vec![0x7f; 4],
                results: vec![0x7f],
            },
        ),
        (
            FREE,
            FuncType {
                params: vec![0x7f; 3],
                results: vec![],
            },
        ),
        (
            RET_SLOT,
            FuncType {
                params: vec![],
                results: vec![0x7f],
            },
        ),
    ];
    for (name, expect) in &runtime {
        match raw.get(*name) {
            Some(got) => check_sig(name, got, expect)?,
            None => {
                return Err(format!(
                    "the module does not export `{name}` (aterm-wasm-abi's allocator and \
                     return slot): invoke `aterm_wasm_abi::export_runtime!()` once in the \
                     module's crate"
                ));
            }
        }
    }

    Ok(Interface {
        classes: classes.into_values().collect(),
        functions,
        imports,
        raw,
    })
}

/// JS reserved words: no class or function at a module's top level may be
/// named so.
pub(crate) const RESERVED: &[&str] = &[
    "arguments",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "eval",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

/// The top-level names the glue itself declares (`templates.rs`, `js.rs`).
const GLUE_NAMES: &[&str] = &[
    "wasm",
    "wasmModule",
    "wasmPath",
    "wasmBytes",
    "initSync",
    "isLikeNone",
    "_assertChar",
    "getStringFromWasm0",
    "decodeText",
    "cachedTextDecoder",
    "MAX_SAFARI_DECODE_BYTES",
    "numBytesDecoded",
    "passStringToWasm0",
    "cachedTextEncoder",
    "WASM_VECTOR_LEN",
    "RET_SLOT",
    "retSlot",
    "checkThrown",
    "getDataViewMemory0",
    "cachedDataViewMemory0",
    "exports",
    "require",
    "module",
    "__dirname",
    "__proto__",
    // Host globals the helpers and loaders read. A public declaration in
    // this module would shadow them even before its first call.
    "Object",
    "Symbol",
    "String",
    "BigInt",
    "Error",
    "FinalizationRegistry",
    "TextDecoder",
    "TextEncoder",
    "DataView",
    "WebAssembly",
    "Response",
    "Request",
    "URL",
    "fetch",
    "performance",
    "Date",
    "console",
    "Uint8Array",
    "Uint16Array",
    "Uint32Array",
    "BigUint64Array",
    "Int8Array",
    "Int16Array",
    "Int32Array",
    "BigInt64Array",
    "Float32Array",
    "Float64Array",
    "InitInput",
    "InitOutput",
    "SyncInitInput",
];

/// Descriptor names become bare JS/TypeScript identifiers, never source.
/// Keep this boundary's identifier alphabet explicit; an unsupported name
/// fails before the caller can publish any glue.
fn identifier(name: &str) -> Result<(), String> {
    let mut bytes = name.bytes();
    let start = |b: u8| b.is_ascii_alphabetic() || b == b'_' || b == b'$';
    if !bytes.next().is_some_and(start) || !bytes.all(|b| start(b) || b.is_ascii_digit()) {
        return Err(format!("`{name}` is not an ASCII JS identifier"));
    }
    Ok(())
}

/// Refuse a class or function name the glue's module scope cannot hold: a
/// JS reserved word, or one of the glue's own names.
fn top_level_name(name: &str) -> Result<(), String> {
    let glue = GLUE_NAMES.contains(&name)
        || name.starts_with("__wbg_")
        || name.ends_with("Finalization")
        || (name.starts_with("getArray") && name.ends_with("FromWasm0"))
        || (name.starts_with("passArray") && name.ends_with("ToWasm0"))
        || ((name.starts_with("get") || name.starts_with("cached")) && name.ends_with("Memory0"));
    if RESERVED.contains(&name) || glue {
        Err(format!(
            "`{name}` cannot name a JS class or function here (a reserved word, or a name \
             the glue declares): rename it with #[export(js_name = ..)] or in Rust"
        ))
    } else {
        Ok(())
    }
}

/// A record as a [`Func`], with its types checked for its kind.
fn func(r: &Record, classes: &BTreeMap<String, Class>) -> Result<Func, String> {
    let at = || {
        if r.class.is_empty() {
            format!("`{}`", r.js_name)
        } else {
            format!("`{}.{}`", r.class, r.js_name)
        }
    };
    let receiver = r.receiver != d::receiver::NONE;
    match r.kind {
        d::kind::FUNCTION if !r.class.is_empty() || receiver => {
            return Err(format!(
                "{}: a free function with a class or receiver",
                at()
            ));
        }
        d::kind::METHOD | d::kind::GETTER if !receiver => {
            return Err(format!("{}: a method without a receiver", at()));
        }
        d::kind::STATIC | d::kind::CONSTRUCTOR if receiver => {
            return Err(format!("{}: a static member with a receiver", at()));
        }
        d::kind::GETTER if !r.params.is_empty() || r.ret == Ty::Unit => {
            return Err(format!(
                "{}: a getter takes nothing and returns a value",
                at()
            ));
        }
        _ => {}
    }
    if r.kind == d::kind::CONSTRUCTOR {
        let own = Ty::Class(r.class.clone());
        let ok = r.ret == own
            || matches!(&r.ret, Ty::Result(ok, err) if **ok == own && **err == Ty::String);
        if !ok {
            return Err(format!(
                "{}: a constructor returns `{}` or `Result<{}, String>`, not `{}`",
                at(),
                r.class,
                r.class,
                r.ret
            ));
        }
    }
    let mut names = BTreeSet::new();
    for (name, ty) in &r.params {
        if !names.insert(name) {
            return Err(format!("{}: two parameters are named `{name}`", at()));
        }
        if ty.param_words().is_none() {
            return Err(format!(
                "{}: parameter `{name}: {ty}` cannot cross from JS",
                at()
            ));
        }
    }
    if r.ret.ret_word().is_none() {
        return Err(format!("{}: `{}` cannot be returned to JS", at(), r.ret));
    }
    let mut class_refs = Vec::new();
    collect_classes(&r.ret, &mut class_refs);
    for c in class_refs {
        if !classes.contains_key(c) {
            return Err(format!(
                "{}: returns class `{c}`, which no #[export] struct declares",
                at()
            ));
        }
    }
    Ok(Func {
        kind: r.kind,
        js_name: r.js_name.clone(),
        symbol: r.symbol.clone(),
        docs: r.docs.clone(),
        receiver,
        params: r.params.clone(),
        ret: r.ret.clone(),
    })
}

fn collect_classes<'a>(ty: &'a Ty, out: &mut Vec<&'a str>) {
    match ty {
        Ty::Class(c) => out.push(c),
        Ty::Option(t) | Ty::Vec(t) | Ty::Slice(t) => collect_classes(t, out),
        Ty::Result(a, b) => {
            collect_classes(a, out);
            collect_classes(b, out);
        }
        _ => {}
    }
}

/// The wasm signature a function's export must have.
pub(crate) fn lowered(f: &Func) -> Result<FuncType, String> {
    let mut params = Vec::new();
    if f.receiver {
        params.push(0x7f);
    }
    for (name, ty) in &f.params {
        let [a, b] = ty
            .param_words()
            .ok_or_else(|| format!("`{}`: parameter `{name}: {ty}` cannot cross", f.js_name))?;
        params.extend([code(a), code(b)]);
    }
    let ret = f
        .ret
        .ret_word()
        .ok_or_else(|| format!("`{}`: `{}` cannot be returned", f.js_name, f.ret))?;
    Ok(FuncType {
        params,
        results: ret.map(code).into_iter().collect(),
    })
}

fn check_sig(symbol: &str, got: &FuncType, expect: &FuncType) -> Result<(), String> {
    if got == expect {
        Ok(())
    } else {
        Err(format!(
            "export `{symbol}` has signature {} but its descriptor lowers to {}: the module and \
             the descriptor disagree (rebuild both from one checkout)",
            show(got),
            show(expect)
        ))
    }
}

fn show(t: &FuncType) -> String {
    let name = |b: &u8| match b {
        0x7f => "i32",
        0x7e => "i64",
        0x7d => "f32",
        0x7c => "f64",
        0x7b => "v128",
        _ => "ref",
    };
    format!(
        "({}) -> ({})",
        t.params.iter().map(name).collect::<Vec<_>>().join(", "),
        t.results.iter().map(name).collect::<Vec<_>>().join(", ")
    )
}
