// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared clang SDK declaration probe ([`the_parser_saw_every_declaration`]).
//! Verified against negative controls ([`a_wrong_prototype_fails_the_probe_on_both_arches`]).

#![cfg(target_os = "macos")]

use std::path::Path;

use crate::sdk_consts;

/// The geometry types the subjects use but do not declare.
const CRATE_GEOMETRY: [&str; 2] = ["CGRect", "CGPoint"];

/// What one file declares, in source order.
#[derive(Default)]
struct Decls {
    /// Zero-sized types behind a pointer (`opaque!`): `struct NAME` in C.
    opaque: Vec<String>,
    /// `(name, rust rhs, target_arch gate)`.
    aliases: Vec<(String, String, Option<String>)>,
    /// `(name, [(field, rust type)])` — `#[repr(C)]` structs with fields.
    structs: Vec<(String, Vec<(String, String)>)>,
    /// `(name, rust type, rust value)`.
    consts: Vec<(String, String, String)>,
    /// `(name, rust type)`.
    statics: Vec<(String, String)>,
    /// `(name, [rust param types], rust return type)`.
    fns: Vec<(String, Vec<String>, String)>,
}

/// `file` under the mounting crate's `src/`.
fn subject(file: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file);
    let src = std::fs::read_to_string(&path).expect("the module is readable");
    src.lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Split at `sep` where no `()`, `[]`, `{}` or `<>` is open.
fn split_top(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0_i32;
    let mut cur = String::new();
    let mut prev = '\0';
    for c in s.chars() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '>' if prev != '-' => depth -= 1,
            _ => {}
        }
        if c == sep && depth == 0 {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
        prev = c;
    }
    out.push(cur);
    out.into_iter()
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
        .collect()
}

/// The top-level items of a file.
fn items(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0_i32;
    let mut cur = String::new();
    for c in src.chars() {
        cur.push(c);
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    out.push(std::mem::take(&mut cur));
                }
            }
            ';' if depth == 0 => out.push(std::mem::take(&mut cur)),
            _ => {}
        }
    }
    out.into_iter()
        .map(|i| i.trim().to_owned())
        .filter(|i| !i.is_empty())
        .collect()
}

/// `(attributes, the item after them)`.
fn strip_attrs(item: &str) -> (Vec<String>, &str) {
    let mut attrs = Vec::new();
    let mut s = item.trim_start();
    while s.starts_with('#') {
        let open = s.find('[').expect("an attribute");
        let mut depth = 0;
        let mut end = open;
        for (i, c) in s[open..].char_indices() {
            match c {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        attrs.push(s[open + 1..end].split_whitespace().collect::<String>());
        s = s[end + 1..].trim_start();
    }
    (attrs, s)
}

/// The braced body of `item`, without the braces.
fn body(item: &str) -> &str {
    let open = item.find('{').expect("a braced body");
    let close = item.rfind('}').expect("a closing brace");
    &item[open + 1..close]
}

/// Strips an optional `name:` prefix from a parameter type.
fn param_type(p: &str) -> String {
    let p = p.trim();
    match p.find(':') {
        Some(i) if !p[i..].starts_with("::") && !p[..i].contains(['*', '<', '(']) => {
            p[i + 1..].trim().to_owned()
        }
        _ => p.to_owned(),
    }
}

/// `fn NAME(params) -> ret` (also an `extern "C" fn(params) -> ret` type).
fn signature(s: &str) -> (Vec<String>, String) {
    let open = s.find('(').expect("a parameter list");
    let mut depth = 0;
    let mut close = open;
    for (i, c) in s[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close = open + i;
                    break;
                }
            }
            _ => {}
        }
    }
    let params = split_top(&s[open + 1..close], ',')
        .iter()
        .map(|p| param_type(p))
        .collect();
    let rest = s[close + 1..].trim();
    let ret = rest
        .strip_prefix("->")
        .map_or_else(|| "()".to_owned(), |r| r.trim().to_owned());
    (params, ret)
}

fn parse(file: &str) -> Decls {
    let mut d = Decls::default();
    for item in items(&subject(file)) {
        let (attrs, rest) = strip_attrs(&item);
        let gate = attrs.iter().find_map(|a| {
            a.strip_prefix("cfg(target_arch=\"")
                .and_then(|a| a.strip_suffix("\")"))
                .map(str::to_owned)
        });
        let repr_c = attrs.iter().any(|a| a == "repr(C)");
        if let Some(r) = rest.strip_prefix("pub type ") {
            let (name, rhs) = r.trim_end_matches(';').split_once('=').expect("an alias");
            d.aliases
                .push((name.trim().to_owned(), rhs.trim().to_owned(), gate));
        } else if let Some(r) = rest
            .strip_prefix("pub const ")
            .filter(|r| !r.starts_with("fn "))
        {
            let (name, r) = r.split_once(':').expect("a typed const");
            let (ty, value) = r.trim_end_matches(';').split_once('=').expect("a value");
            d.consts.push((
                name.trim().to_owned(),
                ty.trim().to_owned(),
                value.trim().to_owned(),
            ));
        } else if let Some(r) = rest.strip_prefix("pub struct ") {
            if repr_c && r.contains('{') {
                let name = r[..r.find('{').unwrap()].trim().to_owned();
                let fields = split_top(body(r), ',')
                    .iter()
                    .map(|f| {
                        let f = f.strip_prefix("pub ").unwrap_or(f);
                        let (n, t) = f.split_once(':').expect("a named field");
                        (n.trim().to_owned(), t.trim().to_owned())
                    })
                    .collect();
                d.structs.push((name, fields));
            }
        } else if rest.contains("opaque!") && !rest.starts_with("macro_rules!") {
            d.opaque.extend(split_top(body(rest), ','));
        } else if rest.starts_with("unsafe extern \"C") {
            for decl in items(body(rest)) {
                let (_, decl) = strip_attrs(&decl);
                let decl = decl.trim_end_matches(';').trim();
                if let Some(r) = decl.strip_prefix("pub static ") {
                    let (name, ty) = r.split_once(':').expect("a typed static");
                    d.statics
                        .push((name.trim().to_owned(), ty.trim().to_owned()));
                } else if let Some(r) = decl.strip_prefix("pub fn ") {
                    let name = r[..r.find('(').expect("a parameter list")]
                        .trim()
                        .to_owned();
                    let (params, ret) = signature(r);
                    d.fns.push((name, params, ret));
                } else {
                    panic!("{file}: an extern item this probe cannot read: {decl:?}");
                }
            }
        }
    }
    d
}

/// Every subject file, merged into one probe.
fn subjects(files: &[&str]) -> Decls {
    let mut all = Decls::default();
    for file in files {
        let d = parse(file);
        all.opaque.extend(d.opaque);
        all.aliases.extend(d.aliases);
        all.structs.extend(d.structs);
        all.consts.extend(d.consts);
        all.statics.extend(d.statics);
        all.fns.extend(d.fns);
    }
    all
}

/// The C type-name for a Rust type, by the rules in the module doc.
fn c_type(rust: &str, d: &Decls) -> String {
    let t = rust.trim();
    let typeof_ = |c: String| {
        if c.contains('(') {
            format!("__typeof__({c})")
        } else {
            c
        }
    };
    if let Some(inner) = t.strip_prefix("*const ") {
        return format!("{} const *", typeof_(c_type(inner, d)));
    }
    if let Some(inner) = t.strip_prefix("*mut ") {
        return format!("{} *", typeof_(c_type(inner, d)));
    }
    if let Some(inner) = t.strip_prefix("Option<").and_then(|i| i.strip_suffix('>')) {
        assert!(
            inner.contains("fn("),
            "Option<{inner}> is not a nullable function pointer"
        );
        return c_type(inner, d);
    }
    if t.starts_with("extern \"C") || t.starts_with("unsafe extern \"C") {
        let (params, ret) = signature(t);
        return format!(
            "__typeof__({}) (*)({})",
            c_type(&ret, d),
            c_params(&params, d)
        );
    }
    let name = t.rsplit("::").next().unwrap_or(t);
    let primitive = match name {
        "()" | "c_void" => Some("void"),
        "c_char" => Some("char"),
        "isize" => Some("long"),
        "usize" => Some("unsigned long"),
        "i8" => Some("int8_t"),
        "u8" => Some("uint8_t"),
        "i16" => Some("int16_t"),
        "u16" => Some("uint16_t"),
        "i32" => Some("int32_t"),
        "u32" => Some("uint32_t"),
        "i64" => Some("int64_t"),
        "u64" => Some("uint64_t"),
        "f32" => Some("float"),
        "f64" => Some("double"),
        _ => None,
    };
    if let Some(p) = primitive {
        return p.to_owned();
    }
    if d.opaque.iter().any(|o| o == name) {
        return format!("struct {name}");
    }
    let known = d.aliases.iter().any(|(n, ..)| n == name)
        || d.structs.iter().any(|(n, _)| n == name)
        || CRATE_GEOMETRY.contains(&name);
    assert!(
        known,
        "no C spelling for the Rust type `{rust}`: teach `c_type` it"
    );
    name.to_owned()
}

fn c_params(params: &[String], d: &Decls) -> String {
    if params.is_empty() {
        "void".to_owned()
    } else {
        params
            .iter()
            .map(|p| c_type(p, d))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The C type of a prototype, for `__typeof__` of the SDK function.
fn c_fn_type(params: &[String], ret: &str, d: &Decls) -> String {
    format!("__typeof__({}) ({})", c_type(ret, d), c_params(params, d))
}

/// The probe.
fn probe(d: &Decls, invert: Option<&str>) -> String {
    let mut s = String::from(
        "#include <CoreFoundation/CoreFoundation.h>\n\
         #include <CoreGraphics/CoreGraphics.h>\n\
         #include <IOKit/IOKitLib.h>\n\
         #include <stddef.h>\n\
         #include <stdint.h>\n\
         #define SAME(a, b) __builtin_types_compatible_p(a, b)\n",
    );
    for (name, rhs, gate) in &d.aliases {
        let line = format!(
            "_Static_assert(SAME({name}, {}), \"type {name}\");\n",
            c_type(rhs, d)
        );
        match gate.as_deref() {
            None => s.push_str(&line),
            Some(arch) => {
                let macro_ = match arch {
                    "aarch64" => "__aarch64__",
                    "x86_64" => "__x86_64__",
                    other => panic!("no C spelling for target_arch {other}"),
                };
                s.push_str(&format!("#if defined({macro_})\n{line}#endif\n"));
            }
        }
    }
    for (name, fields) in &d.structs {
        let mirror = format!("struct aterm_rust_{name}");
        s.push_str(&format!("{mirror} {{\n"));
        for (field, ty) in fields {
            s.push_str(&format!("    __typeof__({}) {field};\n", c_type(ty, d)));
        }
        s.push_str("};\n");
        s.push_str(&format!(
            "_Static_assert(sizeof({name}) == sizeof({mirror}), \"{name} size\");\n\
             _Static_assert(_Alignof({name}) == _Alignof({mirror}), \"{name} align\");\n"
        ));
        for (field, _) in fields {
            s.push_str(&format!(
                "_Static_assert(offsetof({name}, {field}) == offsetof({mirror}, {field}), \
                 \"{name}.{field} offset\");\n\
                 _Static_assert(SAME(__typeof__((({name} *)0)->{field}), \
                 __typeof__((({mirror} *)0)->{field})), \"{name}.{field} type\");\n"
            ));
        }
    }
    for (name, ty, value) in &d.consts {
        let lit = sdk_consts::as_c_literal(value);
        s.push_str(&format!(
            "_Static_assert({name} == ({lit}), \"const {name}\");\n\
             _Static_assert(SAME(__typeof__({name}), {}), \"const {name} type\");\n",
            c_type(ty, d)
        ));
    }
    for (name, ty) in &d.statics {
        s.push_str(&format!(
            "_Static_assert(SAME(__typeof__({name}), {}), \"static {name}\");\n",
            c_type(ty, d)
        ));
    }
    for (name, params, ret) in &d.fns {
        let ret = if invert == Some(name.as_str()) {
            "i8"
        } else {
            ret
        };
        s.push_str(&format!(
            "_Static_assert(SAME(__typeof__({name}), {}), \"fn {name}\");\n",
            c_fn_type(params, ret, d)
        ));
    }
    s.push_str("int main(void) { return 0; }\n");
    s
}

fn compile(arch: &str, body: &str, tag: &str) -> Result<(), String> {
    sdk_consts::compile(
        concat!(env!("CARGO_PKG_NAME"), "-sdk-decls"),
        arch,
        body,
        tag,
    )
}

/// The least each subject declares: `(functions, structs, opaque types)`.
pub struct Floor {
    pub fns: usize,
    pub structs: usize,
    pub opaque: usize,
}

/// Asserts that the parser extracted every declaration in `files` and met `floor`.
pub fn the_parser_saw_every_declaration(files: &[&str], floor: &Floor) {
    let d = subjects(files);
    let count = |needle: &str| -> usize {
        files
            .iter()
            .map(|f| subject(f).matches(needle).count())
            .sum()
    };
    let c_fns: usize = files
        .iter()
        .map(|f| {
            let src = subject(f);
            src.match_indices("fn ")
                .filter(|(i, _)| src[i + 3..].starts_with(|c: char| c.is_ascii_uppercase()))
                .count()
        })
        .sum();
    assert_eq!(d.aliases.len(), count("pub type "), "every alias is a row");
    assert_eq!(
        d.consts.len(),
        count("pub const ") - count("pub const fn "),
        "every constant is a row"
    );
    assert_eq!(
        d.statics.len(),
        count("pub static "),
        "every static is a row"
    );
    assert_eq!(d.fns.len(), c_fns, "every C function is a row");
    assert!(
        d.fns.len() >= floor.fns
            && d.structs.len() >= floor.structs
            && d.opaque.len() >= floor.opaque,
        "the probe read {} functions, {} structs and {} opaque types in {files:?} — fewer \
         than they declare",
        d.fns.len(),
        d.structs.len(),
        d.opaque.len()
    );
}

/// Every declaration in `files` agrees with the SDK, on `arm64` and on `x86_64`.
pub fn every_declaration_matches_the_sdk_on_both_arches(files: &[&str]) {
    let d = subjects(files);
    let body = probe(&d, None);
    for arch in ["arm64", "x86_64"] {
        sdk_consts::green_verdict(
            arch,
            &format!("a declaration in {files:?} disagrees with the SDK"),
            compile(arch, &body, "green"),
        );
    }
}

/// Negative control.
pub fn a_wrong_prototype_fails_the_probe_on_both_arches(files: &[&str], row: &str) {
    let d = subjects(files);
    assert!(
        d.fns.iter().any(|(n, ..)| n == row),
        "{row} is not declared any more; pick another row"
    );
    let body = probe(&d, Some(row));
    for arch in ["arm64", "x86_64"] {
        sdk_consts::inverted_verdict(
            arch,
            &format!("fn {row}"),
            compile(arch, &body, "inverted"),
            &format!(
                "{arch}: the probe accepted `{row}` returning `int8_t` — a wrong return \
                 type compiles, so the green run checks nothing"
            ),
        );
    }
}
