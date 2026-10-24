// Copyright (c) 2014 Alex Crichton
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from wasm-bindgen-cli-support 0.2.108 src/js/mod.rs and
// src/js/binding.rs: the shape of the JS each export's binding writes (the
// argument lowering, the lifting of returns into fresh typed arrays and
// strings with their frees, the class wrap, constructor and
// `Symbol.dispose` alias, and the import shims), so a page written against
// wasm-bindgen's glue sees the same JS. Modified by the aterm project in
// 2026: it is written from aterm-wasm-abi's descriptor, every call is
// followed by the return slot's throw check, and the externref table is
// gone. See ../../LICENSE-MIT for the upstream license; Apache-2.0 is the
// workspace's.

//! The ES module glue (wasm-bindgen's `--target web`), `.d.ts`, and `_bg.wasm.d.ts` for an
//! [`Interface`].

use std::collections::BTreeSet;
use std::fmt::Write as _;

use aterm_wasm_abi::descriptor::{Ty, kind};

use super::model::{ALLOC, FREE, Func, Host, Interface, REALLOC, RESERVED, RET_SLOT};
use super::templates as t;
use super::wasm::FuncType;

/// The three text files of the glue.
pub(crate) struct Glue {
    pub(crate) js: String,
    pub(crate) dts: String,
    pub(crate) bg_dts: String,
}

/// What the module's members use from the helpers.
#[derive(Default)]
struct Needs {
    is_like_none: bool,
    assert_char: bool,
    pass_string: bool,
    get_string: bool,
    ret_slot: bool,
    data_view: bool,
    /// The object table a JS object argument or `aterm_host.object_drop`
    /// uses.
    heap: bool,
    /// The WebGL2 state the `aterm_gl` imports share.
    gl: bool,
    /// Element suffixes (`U8`, `F64`, ..) by direction.
    pass_array: BTreeSet<&'static str>,
    get_array: BTreeSet<&'static str>,
    /// Typed arrays with a cached view.
    views: BTreeSet<&'static str>,
}

/// An element type's helper suffix, typed array and byte size.
const fn elem(ty: &Ty) -> (&'static str, &'static str, usize) {
    match ty {
        Ty::U8 => ("U8", "Uint8Array", 1),
        Ty::I8 => ("I8", "Int8Array", 1),
        Ty::U16 => ("U16", "Uint16Array", 2),
        Ty::I16 => ("I16", "Int16Array", 2),
        Ty::U32 => ("U32", "Uint32Array", 4),
        Ty::I32 => ("I32", "Int32Array", 4),
        Ty::U64 => ("U64", "BigUint64Array", 8),
        Ty::I64 => ("I64", "BigInt64Array", 8),
        Ty::F32 => ("F32", "Float32Array", 4),
        _ => ("F64", "Float64Array", 8),
    }
}

/// The TypeScript type of a value of `ty` as JS receives it.
fn ts(ty: &Ty) -> String {
    match ty {
        Ty::Unit => "void".into(),
        Ty::Bool => "boolean".into(),
        Ty::U64 | Ty::I64 => "bigint".into(),
        Ty::Char | Ty::Str | Ty::String => "string".into(),
        Ty::Slice(e) | Ty::Vec(e) => elem(e).1.into(),
        Ty::Option(inner) => format!("{} | undefined", ts(inner)),
        Ty::Result(ok, _) => ts(ok),
        Ty::Class(c) | Ty::JsObject(c) => c.clone(),
        _ => "number".into(),
    }
}

/// A parameter's TypeScript type and whether it may be left out (an
/// optional with only optionals after it).
fn ts_param(ty: &Ty, trailing: bool) -> (String, bool) {
    match ty {
        Ty::Option(inner) if trailing => (format!("{} | null", ts(inner)), true),
        Ty::Option(inner) => (format!("{} | null | undefined", ts(inner)), false),
        _ => (ts(ty), false),
    }
}

/// The JS names of `f`'s parameters, prefixed with `_` when needed to avoid reserved words, duplicates, or identifiers read by `f`'s body.
fn param_names(f: &Func, class: Option<&str>) -> Vec<String> {
    let placeholders: Vec<String> = (0..f.params.len()).map(|i| format!("arg${i}")).collect();
    let mut code = body(&mut Needs::default(), f, &placeholders, "");
    if let Some(c) = class {
        code = code.replace("{C}", c);
    }
    let read = read_names(&code);
    let mut out: Vec<String> = Vec::with_capacity(f.params.len());
    for (name, _) in &f.params {
        let mut n = name.clone();
        while RESERVED.contains(&n.as_str())
            || read.contains(n.as_str())
            || out.contains(&n)
            || (n != *name && f.params.iter().any(|(other, _)| *other == n))
        {
            n.insert(0, '_');
        }
        out.push(n);
    }
    out
}

/// The non-property identifier tokens `code` reads.
fn read_names(code: &str) -> BTreeSet<&str> {
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    let bytes = code.as_bytes();
    let mut out = BTreeSet::new();
    let mut i = 0;
    while i < bytes.len() {
        if !word(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && word(bytes[i]) {
            i += 1;
        }
        let before = &code[..start];
        let property = before.ends_with('.') && !before.ends_with("..");
        if !bytes[start].is_ascii_digit() && !property {
            out.insert(&code[start..i]);
        }
    }
    out
}

/// Format `text` as a JS string literal with `*/` escaped for header comments.
fn literal(text: &str) -> String {
    aterm_json::to_string(text)
        .expect("a string serializes to JSON")
        .replace("*/", "*\\/")
}

/// A URL path component also quotes percent/query/fragment delimiters in a
/// literal sibling filename; JS string quoting alone does not do that.
pub(super) fn url_component(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// A scalar of `ty` from the wasm result `x`.
fn lift_scalar(ty: &Ty, x: &str) -> String {
    match ty {
        Ty::Bool => format!("{x} !== 0"),
        Ty::U32 | Ty::Usize => format!("{x} >>> 0"),
        Ty::U64 => format!("BigInt.asUintN(64, {x})"),
        Ty::Char => format!("String.fromCodePoint({x})"),
        _ => x.to_string(),
    }
}

impl Needs {
    fn view(&mut self, typed: &'static str) {
        self.views.insert(typed);
    }

    /// Statements that lower parameter `i` (JS name `n`) and the two words
    /// passed for it.
    fn lower(&mut self, i: usize, n: &str, ty: &Ty) -> (Vec<String>, [String; 2]) {
        let zero = "0".to_string();
        match ty {
            Ty::Char => {
                self.assert_char = true;
                (
                    vec![
                        format!("const char${i} = {n}.codePointAt(0);"),
                        format!("_assertChar(char${i});"),
                    ],
                    [format!("char${i}"), zero],
                )
            }
            t if t.is_scalar() => (vec![], [n.to_string(), zero]),
            Ty::JsObject(_) => {
                self.heap = true;
                (vec![], [format!("addHeapObject({n})"), zero])
            }
            Ty::Str | Ty::String => (
                vec![
                    format!("const ptr${i} = {};", self.pass_string_call(n)),
                    format!("const len${i} = WASM_VECTOR_LEN;"),
                ],
                [format!("ptr${i}"), format!("len${i}")],
            ),
            Ty::Slice(e) | Ty::Vec(e) => (
                vec![
                    format!("const ptr${i} = {};", self.pass_array_call(e, n)),
                    format!("const len${i} = WASM_VECTOR_LEN;"),
                ],
                [format!("ptr${i}"), format!("len${i}")],
            ),
            Ty::Option(inner) => {
                self.is_like_none = true;
                let none = format!("isLikeNone({n})");
                match &**inner {
                    Ty::Bool => (
                        vec![],
                        [
                            format!("{none} ? 0 : 1"),
                            format!("{none} ? 0 : {n} ? 1 : 0"),
                        ],
                    ),
                    Ty::U64 | Ty::I64 => (
                        vec![],
                        [
                            format!("{none} ? 0 : 1"),
                            format!("{none} ? BigInt(0) : {n}"),
                        ],
                    ),
                    Ty::Char => {
                        self.assert_char = true;
                        (
                            vec![
                                format!("const char${i} = {none} ? 0xFFFFFF : {n}.codePointAt(0);"),
                                format!("if (char${i} !== 0xFFFFFF) {{ _assertChar(char${i}); }}"),
                            ],
                            [
                                format!("char${i} === 0xFFFFFF ? 0 : 1"),
                                format!("char${i} === 0xFFFFFF ? 0 : char${i}"),
                            ],
                        )
                    }
                    s if s.is_scalar() => (
                        vec![],
                        [format!("{none} ? 0 : 1"), format!("{none} ? 0 : {n}")],
                    ),
                    Ty::Str | Ty::String => (
                        vec![
                            format!("const ptr${i} = {none} ? 0 : {};", self.pass_string_call(n)),
                            format!("const len${i} = WASM_VECTOR_LEN;"),
                        ],
                        [format!("ptr${i}"), format!("len${i}")],
                    ),
                    Ty::Slice(e) | Ty::Vec(e) => (
                        vec![
                            format!(
                                "const ptr${i} = {none} ? 0 : {};",
                                self.pass_array_call(e, n)
                            ),
                            format!("const len${i} = WASM_VECTOR_LEN;"),
                        ],
                        [format!("ptr${i}"), format!("len${i}")],
                    ),
                    _ => (vec![], [zero.clone(), zero]),
                }
            }
            _ => (vec![], [zero.clone(), zero]),
        }
    }

    fn pass_string_call(&mut self, n: &str) -> String {
        self.pass_string = true;
        self.view("Uint8Array");
        format!("passStringToWasm0({n}, wasm.{ALLOC}, wasm.{REALLOC})")
    }

    fn pass_array_call(&mut self, e: &Ty, n: &str) -> String {
        let (s, typed, _) = elem(e);
        self.pass_array.insert(s);
        self.view(typed);
        format!("passArray{s}ToWasm0({n}, wasm.{ALLOC})")
    }

    fn slot(&mut self) -> &'static str {
        self.ret_slot = true;
        self.view("Uint32Array");
        "retSlot()"
    }

    /// Statements that return the call's result `ret$` as JS.
    fn lift(&mut self, ty: &Ty) -> Vec<String> {
        match ty {
            Ty::Result(ok, _) => self.lift(ok),
            s if s.is_scalar() => vec![format!("return {};", lift_scalar(s, "ret$"))],
            Ty::String | Ty::Vec(_) => self.take_buffer(ty),
            Ty::Class(c) => vec![format!("return {c}.__wrap(ret$);")],
            Ty::Option(inner) => match &**inner {
                Ty::Class(c) => vec![format!("return ret$ === 0 ? undefined : {c}.__wrap(ret$);")],
                s if s.is_scalar() => vec![format!(
                    "return {} === 0 ? undefined : {};",
                    self.slot(),
                    lift_scalar(s, "ret$")
                )],
                buffer => {
                    let mut v = vec![
                        "if (ret$ === 0) {".to_string(),
                        "    return undefined;".to_string(),
                        "}".to_string(),
                    ];
                    v.extend(self.take_buffer(buffer));
                    v
                }
            },
            _ => vec![],
        }
    }

    /// Copy the `String`/`Vec` at `ret$` (length in the slot) out and free
    /// it.
    fn take_buffer(&mut self, ty: &Ty) -> Vec<String> {
        let len = format!("const len$ = {};", self.slot());
        match ty {
            Ty::Vec(e) => {
                let (s, typed, size) = elem(e);
                self.get_array.insert(s);
                self.view(typed);
                vec![
                    len,
                    format!("const v$ = getArray{s}FromWasm0(ret$, len$).slice();"),
                    format!("wasm.{FREE}(ret$, len$ * {size}, {size});"),
                    "return v$;".to_string(),
                ]
            }
            _ => {
                self.get_string = true;
                self.view("Uint8Array");
                vec![
                    len,
                    "try {".to_string(),
                    "    return getStringFromWasm0(ret$, len$);".to_string(),
                    "} finally {".to_string(),
                    format!("    wasm.{FREE}(ret$, len$, 1);"),
                    "}".to_string(),
                ]
            }
        }
    }
}

/// A doc comment's lines as `{indent} *{line}`, with `*/` broken.
fn doc_lines(out: &mut String, indent: &str, docs: &str) {
    if docs.is_empty() {
        return;
    }
    for line in docs.split('\n') {
        let line = line.replace("*/", "*\\/");
        let _ = writeln!(out, "{indent} *{}", line.trim_end());
    }
}

/// A JSDoc block: the docs, then `@param` and `@returns` when given.
fn jsdoc(out: &mut String, indent: &str, docs: &str, tags: &[String]) {
    if docs.is_empty() && tags.is_empty() {
        return;
    }
    let _ = writeln!(out, "{indent}/**");
    doc_lines(out, indent, docs);
    for tag in tags {
        let _ = writeln!(out, "{indent} * {tag}");
    }
    let _ = writeln!(out, "{indent} */");
}

/// The JSDoc tags of `f` with JS parameter names `names`.
fn tags(f: &Func, names: &[String], returns: bool) -> Vec<String> {
    let mut out = Vec::new();
    let last_required = f
        .params
        .iter()
        .rposition(|(_, ty)| !matches!(ty, Ty::Option(_)));
    for (i, ((_, ty), n)) in f.params.iter().zip(names).enumerate() {
        let (ts, optional) = ts_param(ty, last_required.is_none_or(|r| i > r));
        out.push(if optional {
            format!("@param {{{ts}}} [{n}]")
        } else {
            format!("@param {{{ts}}} {n}")
        });
    }
    if returns && f.ret != Ty::Unit && !matches!(&f.ret, Ty::Result(ok, _) if **ok == Ty::Unit) {
        out.push(format!("@returns {{{}}}", ts(&f.ret)));
    }
    out
}

/// `f`'s TypeScript parameter list.
fn dts_params(f: &Func, names: &[String]) -> String {
    let last_required = f
        .params
        .iter()
        .rposition(|(_, ty)| !matches!(ty, Ty::Option(_)));
    f.params
        .iter()
        .zip(names)
        .enumerate()
        .map(|(i, ((_, ty), n))| {
            let (ts, optional) = ts_param(ty, last_required.is_none_or(|r| i > r));
            format!("{n}{}: {ts}", if optional { "?" } else { "" })
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The body of a JS function for `f`, indented by `indent`: lower the
/// arguments, call, lift. `this.__wbg_ptr` leads the words of a method.
fn body(needs: &mut Needs, f: &Func, names: &[String], indent: &str) -> String {
    let mut lines = Vec::new();
    let mut words = Vec::new();
    if f.receiver {
        words.push("this.__wbg_ptr".to_string());
    }
    for (i, ((_, ty), n)) in f.params.iter().zip(names).enumerate() {
        let (prep, [a, b]) = needs.lower(i, n, ty);
        lines.extend(prep);
        words.extend([a, b]);
    }
    let call = format!("wasm.{}({})", f.symbol, words.join(", "));
    if f.ret.ret_word() == Some(None) {
        lines.push(format!("{call};"));
        lines.push(t::CHECK_THROWN.to_string());
    } else {
        lines.push(format!("const ret$ = {call};"));
        lines.push(t::CHECK_THROWN.to_string());
        if f.kind == kind::CONSTRUCTOR {
            lines.extend(t::CONSTRUCTOR_TAIL.lines().map(str::to_string));
        } else {
            lines.extend(needs.lift(&f.ret));
        }
    }
    let mut out = String::new();
    for l in lines {
        let _ = writeln!(out, "{indent}{l}");
    }
    out
}

/// Write the glue for `iface`, whose module is `<stem>_bg.wasm`.
pub(crate) fn generate(iface: &Interface, stem: &str) -> Glue {
    let mut needs = Needs::default();
    let mut js = format!(
        "/* @ts-self-types={} */\n\n",
        literal(&format!("./{stem}.d.ts"))
    );
    let mut dts = format!("{}\n", t::DTS_HEADER);

    for c in &iface.classes {
        jsdoc(&mut js, "", &c.docs, &[]);
        let _ = writeln!(js, "export class {} {{", c.name);
        js.push_str(
            &t::CLASS_HEAD
                .replace("{C}", &c.name)
                .replace("{FREE}", &c.free),
        );

        jsdoc(&mut dts, "", &c.docs, &[]);
        let _ = writeln!(dts, "export class {} {{", c.name);
        if c.constructor.is_none() {
            dts.push_str("    private constructor();\n");
        }
        dts.push_str("    free(): void;\n    [Symbol.dispose](): void;\n");

        for f in c.constructor.iter().chain(&c.members) {
            let names = param_names(f, Some(&c.name));
            let params = names.join(", ");
            let (head, dts_line) = match f.kind {
                kind::CONSTRUCTOR => (
                    format!("constructor({params})"),
                    format!("constructor({});", dts_params(f, &names)),
                ),
                kind::GETTER => (
                    format!("get {}()", f.js_name),
                    format!("readonly {}: {};", f.js_name, ts(&f.ret)),
                ),
                kind::STATIC => (
                    format!("static {}({params})", f.js_name),
                    format!(
                        "static {}({}): {};",
                        f.js_name,
                        dts_params(f, &names),
                        ts(&f.ret)
                    ),
                ),
                _ => (
                    format!("{}({params})", f.js_name),
                    format!("{}({}): {};", f.js_name, dts_params(f, &names), ts(&f.ret)),
                ),
            };
            jsdoc(
                &mut js,
                "    ",
                &f.docs,
                &tags(f, &names, f.kind != kind::CONSTRUCTOR),
            );
            let _ = writeln!(js, "    {head} {{");
            js.push_str(&body(&mut needs, f, &names, "        ").replace("{C}", &c.name));
            js.push_str("    }\n");

            jsdoc(&mut dts, "    ", &f.docs, &[]);
            let _ = writeln!(dts, "    {dts_line}");
        }
        js.push_str("}\n");
        js.push_str(&t::DISPOSE_ALIAS.replace("{C}", &c.name));
        js.push('\n');
        dts.push_str("}\n\n");
    }

    for f in &iface.functions {
        let names = param_names(f, None);
        jsdoc(&mut js, "", &f.docs, &tags(f, &names, true));
        let _ = writeln!(js, "export function {}({}) {{", f.js_name, names.join(", "));
        js.push_str(&body(&mut needs, f, &names, "    "));
        js.push_str("}\n");
        js.push('\n');

        jsdoc(&mut dts, "", &f.docs, &[]);
        let _ = writeln!(
            dts,
            "export function {}({}): {};\n",
            f.js_name,
            dts_params(f, &names),
            ts(&f.ret)
        );
    }

    js.push_str(&imports(&mut needs, &iface.imports));

    for c in &iface.classes {
        js.push_str(
            &t::FINALIZATION
                .replace("{C}", &c.name)
                .replace("{FREE}", &c.free),
        );
    }
    js.push('\n');

    let slot_init = format!("RET_SLOT = wasm.{RET_SLOT}() >>> 0;\n");
    js.push_str(&helpers(&mut needs));

    let mut reset = String::new();
    if needs.data_view {
        reset.push_str("    cachedDataViewMemory0 = null;\n");
    }
    for typed in &needs.views {
        let _ = writeln!(reset, "    cached{typed}Memory0 = null;");
    }
    let init = format!("    {slot_init}");
    js.push_str(
        &t::WEB_LOADER
            .replace(
                "{FILE}",
                &literal(&url_component(&format!("{stem}_bg.wasm"))),
            )
            .replace("{RESET}", &reset)
            .replace("{INIT}", &init),
    );
    dts = dts.trim_end().to_string() + "\n\n";
    dts.push_str(t::WEB_DTS_INPUT);
    dts.push_str("\nexport interface InitOutput {\n");
    dts.push_str("    readonly memory: WebAssembly.Memory;\n");
    for (name, ty) in &iface.raw {
        let _ = writeln!(dts, "    readonly {name}: {};", raw_ts(ty));
    }
    dts.push_str("}\n\n");
    dts.push_str(t::WEB_DTS_INIT);

    let mut bg_dts = String::from(t::DTS_HEADER);
    bg_dts.push_str("export const memory: WebAssembly.Memory;\n");
    for (name, ty) in &iface.raw {
        let _ = writeln!(bg_dts, "export const {name}: {};", raw_ts(ty));
    }

    Glue {
        js,
        dts: dts.trim_end().to_string() + "\n",
        bg_dts,
    }
}

/// `__wbg_get_imports`: an object per import module with the JS behind
/// each import the module makes.
fn imports(needs: &mut Needs, hosts: &[Host]) -> String {
    let mut sorted: Vec<Host> = hosts.to_vec();
    sorted.sort_by_key(|h| (h.spec().0, h.spec().1));
    let mut modules: Vec<(&str, Vec<String>)> = Vec::new();
    for h in sorted {
        let (module, name, _, _) = h.spec();
        let body = match h {
            Host::ConsoleError => {
                needs.get_string = true;
                needs.view("Uint8Array");
                t::CONSOLE_ERROR_SHIM.to_string()
            }
            Host::ErrorStack => {
                needs.data_view = true;
                let pass = needs.pass_string_call("ret");
                t::ERROR_STACK_SHIM.replace("{PASS}", &pass)
            }
            Host::PerformanceNow => t::PERFORMANCE_NOW_SHIM.to_string(),
            Host::DateNow => t::DATE_NOW_SHIM.to_string(),
            Host::RandomFill => t::RANDOM_FILL_SHIM.to_string(),
            Host::ObjectDrop => {
                needs.heap = true;
                t::OBJECT_DROP_SHIM.to_string()
            }
            Host::Gl(i) => {
                needs.gl = true;
                needs.heap = true;
                needs.data_view = true;
                for typed in super::webgl::VIEWS {
                    needs.view(typed);
                }
                super::webgl::IMPORTS[i].body.to_string()
            }
        };
        let entry = format!("        {}: {body},", js_key(name));
        match modules.iter_mut().find(|(m, _)| *m == module) {
            Some((_, v)) => v.push(entry),
            None => modules.push((module, vec![entry])),
        }
    }
    let mut out = String::from("function __wbg_get_imports() {\n");
    for (i, (_, entries)) in modules.iter().enumerate() {
        let _ = writeln!(out, "    const import{i} = {{\n        __proto__: null,");
        for e in entries {
            out.push_str(e);
            out.push('\n');
        }
        out.push_str("    };\n");
    }
    out.push_str("    return {\n        __proto__: null,\n");
    for (i, (module, _)) in modules.iter().enumerate() {
        let _ = writeln!(out, "        {:?}: import{i},", module);
    }
    out.push_str("    };\n}\n\n");
    out
}

/// An object key: bare when it is an identifier and no reserved word.
fn js_key(name: &str) -> String {
    if RESERVED.contains(&name) {
        format!("{name:?}")
    } else {
        name.to_string()
    }
}

/// The helpers the members asked for, in a fixed order.
fn helpers(needs: &mut Needs) -> String {
    // `checkThrown` unconditionally requires `getStringFromWasm0`, `Uint32Array`, and `Uint8Array`.
    needs.get_string = true;
    needs.view("Uint32Array");
    needs.view("Uint8Array");
    let mut out = String::new();
    if needs.assert_char {
        out.push_str(t::ASSERT_CHAR);
        out.push('\n');
    }
    for s in needs.get_array.clone() {
        let (typed, n) = suffix_info(s);
        out.push_str(
            &t::GET_ARRAY
                .replace("{S}", s)
                .replace("{T}", typed)
                .replace("{N}", &n.to_string()),
        );
        out.push('\n');
    }
    if needs.data_view {
        out.push_str(t::DATA_VIEW);
        out.push('\n');
    }
    if needs.get_string {
        out.push_str(&t::GET_STRING.replace("{DECODE}", t::DECODE_TEXT_WEB));
        out.push('\n');
    }
    for typed in &needs.views {
        out.push_str(&t::VIEW.replace("{T}", typed));
        out.push('\n');
    }
    if needs.is_like_none {
        out.push_str(t::IS_LIKE_NONE);
        out.push('\n');
    }
    if needs.heap {
        out.push_str(t::HEAP);
        out.push('\n');
    }
    if needs.gl {
        out.push_str(super::webgl::STATE);
        out.push('\n');
    }
    for s in needs.pass_array.clone() {
        let (typed, n) = suffix_info(s);
        out.push_str(
            &t::PASS_ARRAY
                .replace("{S}", s)
                .replace("{T}", typed)
                .replace("{N}", &n.to_string()),
        );
        out.push('\n');
    }
    if needs.pass_string {
        out.push_str(t::PASS_STRING);
        out.push('\n');
    }
    out.push_str(
        &t::RET_SLOT
            .replace("{FREE}", FREE)
            .replace("{ERROR}", &aterm_wasm_abi::mem::thrown::ERROR.to_string()),
    );
    out.push('\n');
    if needs.ret_slot {
        out.push_str(t::RET_VALUE);
        out.push('\n');
    }
    if needs.pass_string || !needs.pass_array.is_empty() {
        out.push_str(t::VECTOR_LEN);
        out.push('\n');
    }
    out
}

fn suffix_info(s: &str) -> (&'static str, usize) {
    let ty = match s {
        "U8" => Ty::U8,
        "I8" => Ty::I8,
        "U16" => Ty::U16,
        "I16" => Ty::I16,
        "U32" => Ty::U32,
        "I32" => Ty::I32,
        "U64" => Ty::U64,
        "I64" => Ty::I64,
        "F32" => Ty::F32,
        _ => Ty::F64,
    };
    let (_, typed, n) = elem(&ty);
    (typed, n)
}

/// A raw export's TypeScript function type.
fn raw_ts(ty: &FuncType) -> String {
    let one = |b: &u8| match b {
        0x7f | 0x7d | 0x7c => "number",
        0x7e => "bigint",
        _ => "any",
    };
    let params: Vec<String> = ty
        .params
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let letter = char::from(b'a' + (i % 26) as u8);
            let name = if i < 26 {
                letter.to_string()
            } else {
                format!("{letter}{}", i / 26)
            };
            format!("{name}: {}", one(b))
        })
        .collect();
    let ret = match ty.results.as_slice() {
        [] => "void".to_string(),
        [one_ty] => one(one_ty).to_string(),
        many => format!("[{}]", many.iter().map(one).collect::<Vec<_>>().join(", ")),
    };
    format!("({}) => {ret}", params.join(", "))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::wasm_glue::model::Host;
    use crate::wasm_glue::webgl;

    /// The GL import at `name` in the table.
    fn gl(name: &str) -> Host {
        Host::Gl(
            webgl::IMPORTS
                .iter()
                .position(|g| g.name == name)
                .expect("a listed import"),
        )
    }

    /// A JS object argument is stored in the object table and crosses as its
    /// index, typed in the `.d.ts` by the name its descriptor carries; the
    /// `aterm_gl` imports bring the WebGL2 state and the table they read the
    /// canvas from, and a module importing neither gets none of it.
    #[test]
    fn a_js_object_crosses_by_its_table_slot_and_gl_imports_bring_their_state() {
        let init = Func {
            kind: kind::FUNCTION,
            js_name: "init".into(),
            symbol: "init".into(),
            docs: String::new(),
            receiver: false,
            params: vec![("canvas".into(), Ty::JsObject("HTMLCanvasElement".into()))],
            ret: Ty::Unit,
        };
        let iface = Interface {
            classes: Vec::new(),
            functions: vec![init.clone()],
            imports: vec![Host::ObjectDrop, gl("context_create"), gl("make_current")],
            raw: BTreeMap::new(),
        };
        let g = generate(&iface, "m");
        assert!(
            g.js.contains("wasm.init(addHeapObject(canvas), 0);"),
            "{}",
            g.js
        );
        assert!(
            g.dts
                .contains("export function init(canvas: HTMLCanvasElement): void;")
        );
        for want in [
            "function addHeapObject(obj)",
            "dropObject(arg0);",
            "const glContexts = [null];",
            "canvas.getContext('webgl2', { antialias: false })",
            "make_current: function(arg0)",
            "cachedFloat32ArrayMemory0 = null;",
        ] {
            assert!(g.js.contains(want), "the glue lacks {want:?}");
        }
        let plain = Interface {
            imports: Vec::new(),
            functions: vec![Func {
                params: Vec::new(),
                ..init
            }],
            ..iface
        };
        let g = generate(&plain, "m");
        assert!(!g.js.contains("addHeapObject") && !g.js.contains("GLctx"));
    }

    /// The GL-lifetime fixture's glue: the generator on the imports
    /// `gl-lifetimes.mjs` drives.
    #[cfg(unix)]
    fn lifetime_glue() -> String {
        let iface = Interface {
            classes: Vec::new(),
            functions: vec![Func {
                kind: kind::FUNCTION,
                js_name: "create_context".into(),
                symbol: "create_context".into(),
                docs: String::new(),
                receiver: false,
                params: vec![("canvas".into(), Ty::JsObject("HTMLCanvasElement".into()))],
                ret: Ty::U32,
            }],
            imports: std::iter::once(Host::ObjectDrop)
                .chain(
                    [
                        "context_create",
                        "context_drop",
                        "make_current",
                        "glGetString",
                        "glCreateProgram",
                        "glDeleteProgram",
                        "glGetUniformLocation",
                        "glUniform1i",
                    ]
                    .into_iter()
                    .map(gl),
                )
                .collect(),
            raw: BTreeMap::new(),
        };
        generate(&iface, "lifetime").js
    }

    /// Run `gl-lifetimes.mjs` on `js` in a headless page and return whether it held along with console output.
    #[cfg(unix)]
    fn drive_lifetimes(js: &str, keep: bool) -> (bool, String) {
        let dir = aterm_tempfile::Builder::new()
            .prefix("atv-gl-life-")
            .tempdir()
            .expect("exclusive fixture root");
        std::fs::write(dir.path().join("lifetime.js"), js).expect("owned generated glue");
        std::fs::write(
            dir.path().join("driver.mjs"),
            include_str!("../../browser/gl-lifetimes.mjs"),
        )
        .expect("owned driver");
        let mut page = crate::wasm_page::start_for_test(dir.path());
        let ran = page.run("driver.mjs", &aterm_json::json!({"glue": "./lifetime.js"}));
        let console = match &ran {
            Ok(ran) => ran.console.join("\n"),
            Err(refused) => refused.why().to_string(),
        };
        eprintln!("{console}");
        if ran.is_err() && keep {
            drop(page);
            eprintln!(
                "GL_LIFETIME_OWNED_FIXTURE_RETAINED: {}",
                dir.keep().display()
            );
        }
        (ran.is_ok(), console)
    }

    /// Generated imports retire only their context or program's owned strings and uniform locations.
    #[cfg(unix)]
    #[test]
    fn context_strings_and_program_locations_retire_with_their_owner() {
        let (held, console) = drive_lifetimes(&lifetime_glue(), true);
        assert!(
            console.contains("GL_LIFETIME_ADMITTED_CONTROLS_REACHED"),
            "fixture controls not reached: {console}"
        );
        assert!(
            console.contains("GL_LIFETIME_RETIREMENT_CASES_REACHED"),
            "retirement cases not reached: {console}"
        );
        assert!(
            held && console.contains("GL_LIFETIME_ALL_RETIRED"),
            "GL_LIFETIME_RETIREMENT_MISSING: {console}"
        );
    }

    /// Glue that reaches the network on import or a real canvas GPU context in `context_create` fails the fixture fence.
    #[cfg(unix)]
    #[test]
    fn glue_that_reaches_the_network_or_a_real_gpu_fails_the_gl_fixture() {
        let js = lifetime_glue();
        let real_canvas = "canvas.getContext('webgl2', { antialias: false })";
        assert!(js.contains(real_canvas), "{js}");
        for (planted, reached) in [
            (format!("void fetch;\n{js}"), "fetch"),
            (
                js.replace(
                    real_canvas,
                    "document.createElement('canvas').getContext('webgl2', { antialias: false })",
                ),
                "getContext",
            ),
        ] {
            let (held, console) = drive_lifetimes(&planted, false);
            assert!(
                !held
                    && console.contains(&format!("GL_LIFETIME_FENCE: the glue reached {reached}")),
                "{reached}: {console}"
            );
        }
    }
}
