// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The protocol model read from the XML, and the Rust written from it.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

use crate::xml::{self, Element};

/// The protocol files, in the order their modules are listed.
pub const FILES: &[(&str, &str)] = &[
    ("wayland.xml", "COPYING.wayland"),
    ("xdg-shell.xml", "COPYING.wayland-protocols"),
    (
        "xdg-decoration-unstable-v1.xml",
        "COPYING.wayland-protocols",
    ),
    ("xdg-activation-v1.xml", "COPYING.wayland-protocols"),
    ("viewporter.xml", "COPYING.wayland-protocols"),
    ("fractional-scale-v1.xml", "COPYING.wayland-protocols"),
    ("cursor-shape-v1.xml", "COPYING.wayland-protocols"),
    ("text-input-unstable-v3.xml", "COPYING.wayland-protocols"),
    (
        "primary-selection-unstable-v1.xml",
        "COPYING.wayland-protocols",
    ),
];

const NULLS: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Int,
    Uint,
    Fixed,
    Str,
    Object,
    NewId,
    Array,
    Fd,
}

struct Arg {
    name: String,
    kind: Kind,
    interface: Option<String>,
    allow_null: bool,
}

struct Msg {
    name: String,
    since: u32,
    destructor: bool,
    summary: String,
    args: Vec<Arg>,
}

struct Enum {
    name: String,
    entries: Vec<(String, u32, String)>,
}

struct Iface {
    name: String,
    version: u32,
    summary: String,
    requests: Vec<Msg>,
    events: Vec<Msg>,
    enums: Vec<Enum>,
}

struct Protocol {
    name: String,
    file: String,
    copying: String,
    copyright: Vec<String>,
    interfaces: Vec<Iface>,
}

fn summary(e: &Element) -> String {
    e.attr("summary")
        .map(str::to_owned)
        .or_else(|| {
            e.children("description")
                .next()
                .and_then(|d| d.attr("summary").map(str::to_owned))
        })
        .unwrap_or_default()
}

fn number(s: &str) -> Result<u32, String> {
    match s.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => s.parse(),
    }
    .map_err(|e| format!("{s:?}: {e}"))
}

fn read_msg(e: &Element) -> Result<Msg, String> {
    let name = e.attr("name").ok_or("a message without a name")?.to_owned();
    let mut args = Vec::new();
    for a in e.children("arg") {
        let kind = match a.attr("type").ok_or("an arg without a type")? {
            "int" => Kind::Int,
            "uint" => Kind::Uint,
            "fixed" => Kind::Fixed,
            "string" => Kind::Str,
            "object" => Kind::Object,
            "new_id" => Kind::NewId,
            "array" => Kind::Array,
            "fd" => Kind::Fd,
            other => return Err(format!("{name}: arg type {other}")),
        };
        args.push(Arg {
            name: a.attr("name").ok_or("an arg without a name")?.to_owned(),
            kind,
            interface: a.attr("interface").map(str::to_owned),
            allow_null: a.attr("allow-null") == Some("true"),
        });
    }
    Ok(Msg {
        since: e.attr("since").map(number).transpose()?.unwrap_or(1),
        destructor: e.attr("type") == Some("destructor"),
        summary: summary(e),
        name,
        args,
    })
}

fn read(dir: &Path, file: &str, copying: &str) -> Result<Protocol, String> {
    let src = std::fs::read_to_string(dir.join(file)).map_err(|e| format!("{file}: {e}"))?;
    let root = xml::parse(&src).map_err(|e| format!("{file}: {e}"))?;
    if root.name != "protocol" {
        return Err(format!("{file}: root <{}>", root.name));
    }
    let copyright = root
        .children("copyright")
        .flat_map(|c| c.text.lines())
        .map(str::trim)
        .filter(|l| l.starts_with("Copyright"))
        .map(str::to_owned)
        .collect();
    let mut interfaces = Vec::new();
    for i in root.children("interface") {
        let name = i
            .attr("name")
            .ok_or("an interface without a name")?
            .to_owned();
        let mut enums = Vec::new();
        for en in i.children("enum") {
            let mut entries = Vec::new();
            for entry in en.children("entry") {
                entries.push((
                    entry
                        .attr("name")
                        .ok_or("an entry without a name")?
                        .to_owned(),
                    number(entry.attr("value").ok_or("an entry without a value")?)?,
                    summary(entry),
                ));
            }
            enums.push(Enum {
                name: en.attr("name").ok_or("an enum without a name")?.to_owned(),
                entries,
            });
        }
        interfaces.push(Iface {
            version: number(i.attr("version").ok_or("an interface without a version")?)?,
            summary: summary(i),
            requests: i
                .children("request")
                .map(read_msg)
                .collect::<Result<_, _>>()?,
            events: i
                .children("event")
                .map(read_msg)
                .collect::<Result<_, _>>()?,
            enums,
            name,
        });
    }
    Ok(Protocol {
        name: root
            .attr("name")
            .ok_or("a protocol without a name")?
            .to_owned(),
        file: file.to_owned(),
        copying: copying.to_owned(),
        copyright,
        interfaces,
    })
}

fn signature(m: &Msg) -> String {
    let mut s = String::new();
    if m.since > 1 {
        s.push_str(&m.since.to_string());
    }
    for a in &m.args {
        let nullable = matches!(a.kind, Kind::Str | Kind::Object | Kind::NewId | Kind::Array);
        if nullable && a.allow_null {
            s.push('?');
        }
        s.push_str(match a.kind {
            Kind::Int => "i",
            Kind::Uint => "u",
            Kind::Fixed => "f",
            Kind::Str => "s",
            Kind::Object => "o",
            Kind::NewId if a.interface.is_none() => "sun",
            Kind::NewId => "n",
            Kind::Array => "a",
            Kind::Fd => "h",
        });
    }
    s
}

const KEYWORDS: &[&str] = &[
    "as", "async", "await", "box", "break", "const", "continue", "crate", "do", "dyn", "else",
    "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl", "in", "let", "loop",
    "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref", "return", "static",
    "struct", "super", "trait", "true", "try", "type", "typeof", "unsafe", "unsized", "use",
    "virtual", "where", "while", "yield", "abstract", "become",
];

fn ident(name: &str) -> String {
    if KEYWORDS.contains(&name) {
        format!("r#{name}")
    } else {
        name.to_owned()
    }
}

fn konst(name: &str) -> String {
    let up = name.to_ascii_uppercase();
    if up.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{up}")
    } else {
        up
    }
}

fn doc(s: &str) -> String {
    let words = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::with_capacity(words.len());
    for c in words.chars() {
        if matches!(c, '[' | ']' | '<' | '>' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn paths(protocols: &[Protocol]) -> Vec<(String, String)> {
    protocols
        .iter()
        .flat_map(|p| {
            p.interfaces.iter().map(move |i| {
                (
                    i.name.clone(),
                    format!("crate::protocols::{}::{}", p.name, i.name),
                )
            })
        })
        .collect()
}

fn path_of(paths: &[(String, String)], name: &str) -> String {
    paths.iter().find(|(n, _)| n == name).map_or_else(
        || format!("crate::protocols::stubs::{name}"),
        |(_, p)| p.clone(),
    )
}

fn messages(
    out: &mut String,
    paths: &[(String, String)],
    which: &str,
    msgs: &[Msg],
) -> Result<(), String> {
    for m in msgs {
        let sig = signature(m);
        if m.args
            .iter()
            .all(|a| a.interface.is_none() || !matches!(a.kind, Kind::Object | Kind::NewId))
        {
            if crate::generate::sig_args(&sig) > NULLS {
                return Err(format!("{}: more than {NULLS} untyped arguments", m.name));
            }
            continue;
        }
        let _ = writeln!(
            out,
            "    static {}_{}_TYPES: [TypeRef; {}] = [",
            which,
            konst(&m.name),
            sig_args(&sig)
        );
        for a in &m.args {
            match (a.kind, &a.interface) {
                (Kind::Object | Kind::NewId, Some(i)) => {
                    let _ = writeln!(
                        out,
                        "        TypeRef::to(&{}::INTERFACE),",
                        path_of(paths, i)
                    );
                }
                (Kind::NewId, None) => {
                    out.push_str(
                        "        TypeRef::NULL,\n        TypeRef::NULL,\n        TypeRef::NULL,\n",
                    );
                }
                _ => out.push_str("        TypeRef::NULL,\n"),
            }
        }
        out.push_str("    ];\n");
    }
    let _ = writeln!(out, "    static {which}: [Message; {}] = [", msgs.len());
    for m in msgs {
        let sig = signature(m);
        let typed = m
            .args
            .iter()
            .any(|a| a.interface.is_some() && matches!(a.kind, Kind::Object | Kind::NewId));
        let types = if typed {
            format!("&{}_{}_TYPES", which, konst(&m.name))
        } else {
            "&NULLS".to_owned()
        };
        let _ = writeln!(
            out,
            "        Message::new(c\"{}\", c\"{sig}\", {types}),",
            m.name
        );
    }
    out.push_str("    ];\n");
    Ok(())
}

pub fn sig_args(sig: &str) -> usize {
    sig.bytes()
        .filter(|c| matches!(c, b'i' | b'u' | b'f' | b's' | b'o' | b'n' | b'a' | b'h'))
        .count()
}

fn request_fn(out: &mut String, paths: &[(String, String)], iface: &Iface, opcode: usize, m: &Msg) {
    let mut params = vec!["this: Proxy".to_owned()];
    let mut args = Vec::new();
    let mut prelude = Vec::new();
    let mut new = "None".to_owned();
    let mut returns = false;
    for a in &m.args {
        let n = ident(&a.name);
        match a.kind {
            Kind::Int => {
                params.push(format!("{n}: i32"));
                args.push(format!("Arg::int({n})"));
            }
            Kind::Uint => {
                params.push(format!("{n}: u32"));
                args.push(format!("Arg::uint({n})"));
            }
            Kind::Fixed => {
                params.push(format!("{n}: Fixed"));
                args.push(format!("Arg::fixed({n})"));
            }
            Kind::Str if a.allow_null => {
                params.push(format!("{n}: Option<&CStr>"));
                args.push(format!("Arg::string({n})"));
            }
            Kind::Str => {
                params.push(format!("{n}: &CStr"));
                args.push(format!("Arg::string(Some({n}))"));
            }
            Kind::Object if a.allow_null => {
                params.push(format!("{n}: Option<Proxy>"));
                args.push(format!("Arg::object({n})"));
            }
            Kind::Object => {
                params.push(format!("{n}: Proxy"));
                args.push(format!("Arg::object(Some({n}))"));
            }
            Kind::NewId => match &a.interface {
                Some(i) => {
                    new = format!("Some((&{}::INTERFACE, this.version()))", path_of(paths, i));
                    args.push("Arg::new_id()".to_owned());
                    returns = true;
                }
                None => {
                    params.push("interface: &'static Interface".to_owned());
                    params.push("version: u32".to_owned());
                    args.push("Arg::string(Some(interface.name()))".to_owned());
                    args.push("Arg::uint(version)".to_owned());
                    args.push("Arg::new_id()".to_owned());
                    new = "Some((interface, version))".to_owned();
                    returns = true;
                }
            },
            Kind::Array => {
                params.push(format!("{n}: &[u8]"));
                prelude.push(format!("let mut {n} = borrowed_array({n});"));
                args.push(format!("Arg::array(&mut {n})"));
            }
            Kind::Fd => {
                params.push(format!("{n}: c_int"));
                args.push(format!("Arg::fd({n})"));
            }
        }
    }
    if !m.summary.is_empty() {
        let _ = writeln!(out, "    /// {}", doc(&m.summary));
        out.push_str("    ///\n");
    }
    out.push_str("    /// # Safety\n");
    let _ = writeln!(
        out,
        "    /// `this` is a live `{}`{}; every object argument is live.",
        iface.name,
        if m.destructor {
            ", destroyed by this request and not used again"
        } else {
            ""
        }
    );
    let ret = if returns { " -> Option<Proxy>" } else { "" };
    let _ = writeln!(
        out,
        "    pub unsafe fn {}({}){ret} {{",
        ident(&m.name),
        params.join(", ")
    );
    for p in &prelude {
        let _ = writeln!(out, "        {p}");
    }
    out.push_str("        // SAFETY: the caller's contract; the arguments follow the signature.\n");
    let call = format!(
        "unsafe {{ this.marshal({opcode}, &mut [{}], {new}, {}) }}",
        args.join(", "),
        m.destructor
    );
    if returns {
        let _ = writeln!(out, "        {call}");
    } else {
        let _ = writeln!(out, "        let _ = {call};");
    }
    out.push_str("    }\n");
}

fn interface(out: &mut String, paths: &[(String, String)], i: &Iface) -> Result<(), String> {
    if !i.summary.is_empty() {
        let _ = writeln!(out, "/// {}", doc(&i.summary));
    }
    let _ = writeln!(out, "pub mod {} {{", ident(&i.name));
    out.push_str("    #[allow(unused_imports)]\n");
    out.push_str("    use crate::iface::{Interface, Message, NULLS, TypeRef};\n");
    out.push_str("    #[allow(unused_imports)]\n");
    out.push_str("    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};\n");
    out.push_str("    #[allow(unused_imports)]\n");
    out.push_str("    use std::ffi::{CStr, c_int};\n\n");
    out.push_str("    /// The highest version this table describes.\n");
    let _ = writeln!(out, "    pub const VERSION: u32 = {};", i.version);
    out.push_str("    /// The interface table.\n");
    let _ = writeln!(
        out,
        "    pub static INTERFACE: Interface = Interface::new(c\"{}\", {}, &REQUESTS, &EVENTS);",
        i.name, i.version
    );
    messages(out, paths, "REQUESTS", &i.requests)?;
    messages(out, paths, "EVENTS", &i.events)?;
    for (which, msgs) in [("req", &i.requests), ("evt", &i.events)] {
        let _ = writeln!(
            out,
            "    /// {} opcodes.",
            if which == "req" { "Request" } else { "Event" }
        );
        let _ = writeln!(out, "    pub mod {which} {{");
        for (n, m) in msgs.iter().enumerate() {
            let _ = writeln!(out, "        pub const {}: u32 = {n};", konst(&m.name));
        }
        out.push_str("    }\n");
    }
    for e in &i.enums {
        let _ = writeln!(out, "    /// The `{}` enum.", e.name);
        let _ = writeln!(out, "    pub mod {} {{", ident(&e.name));
        for (name, value, s) in &e.entries {
            if !s.is_empty() {
                let _ = writeln!(out, "        /// {}", doc(s));
            }
            let _ = writeln!(out, "        pub const {}: u32 = {value:#x};", konst(name));
        }
        out.push_str("    }\n");
    }
    for (n, m) in i.requests.iter().enumerate() {
        request_fn(out, paths, i, n, m);
    }
    out.push_str("}\n\n");
    Ok(())
}

fn header(p: &Protocol) -> String {
    let mut h = String::from("// SPDX-License-Identifier: MIT\n// Copyright 2026 Andrew Yates\n");
    for line in &p.copyright {
        let _ = writeln!(h, "// {line}");
    }
    let _ = write!(
        h,
        "//\n// Generated from protocols/{} (MIT, see protocols/{}) by\n\
         // `targo --unverified run --release` in crates/aterm-wayland/scan. Do not edit.\n\n\
         //! The `{}` protocol.\n\n{OPEN}",
        p.file, p.copying, p.name
    );
    h
}

const OPEN: &str = "pub use self::tables::*;\n\n#[rustfmt::skip]\nmod tables {\n";
const CLOSE: &str = "}\n";

/// Every generated file, as (path relative to the crate, text).
pub fn generate(root: &Path) -> Result<Vec<(String, String)>, String> {
    let dir = root.join("protocols");
    let protocols = FILES
        .iter()
        .map(|(f, c)| read(&dir, f, c))
        .collect::<Result<Vec<_>, _>>()?;
    let paths = paths(&protocols);
    let mut out = Vec::new();
    let mut referenced = BTreeSet::new();
    for p in &protocols {
        let mut text = header(p);
        for i in &p.interfaces {
            for m in i.requests.iter().chain(&i.events) {
                referenced.extend(m.args.iter().filter_map(|a| a.interface.clone()));
            }
            interface(&mut text, &paths, i)?;
        }
        let text = text.trim_end().to_owned() + "\n" + CLOSE;
        out.push((format!("src/protocols/{}.rs", p.name), text));
    }
    let stubs: Vec<_> = referenced
        .into_iter()
        .filter(|n| !paths.iter().any(|(m, _)| m == n))
        .collect();
    let mut s = String::from(
        "// SPDX-License-Identifier: Apache-2.0\n// Copyright 2026 Andrew Yates\n//\n\
         // Generated by `targo --unverified run --release` in crates/aterm-wayland/scan. Do not edit.\n\n\
         //! Empty tables for interfaces a protocol here names but no file here\n\
         //! defines: a message's argument type is checked by name, so a stub of\n\
         //! the right name is all libwayland reads of it.\n\n",
    );
    s.push_str(OPEN);
    for n in &stubs {
        let _ = writeln!(s, "/// `{n}`, named and nothing more.");
        let _ = writeln!(s, "pub mod {} {{", ident(n));
        out_stub(&mut s, n);
        s.push_str("}\n");
    }
    s.push_str(CLOSE);
    out.push(("src/protocols/stubs.rs".to_owned(), s));
    let mut m = String::from(
        "// SPDX-License-Identifier: Apache-2.0\n// Copyright 2026 Andrew Yates\n//\n\
         // Generated by `targo --unverified run --release` in crates/aterm-wayland/scan. Do not edit.\n\n\
         //! The generated protocol modules, one per file under `protocols/`.\n\n",
    );
    let mut mods: Vec<&str> = protocols.iter().map(|p| p.name.as_str()).collect();
    mods.push("stubs");
    mods.sort_unstable();
    for name in mods {
        let _ = writeln!(m, "pub mod {name};");
    }
    m.push_str("\npub use self::all::ALL;\n\n#[rustfmt::skip]\nmod all {\n/// Every interface table, in file order.\npub static ALL: &[&crate::iface::Interface] = &[\n");
    for p in &protocols {
        for i in &p.interfaces {
            let _ = writeln!(
                m,
                "    &crate::protocols::{}::{}::INTERFACE,",
                p.name,
                ident(&i.name)
            );
        }
    }
    m.push_str("];\n}\n");
    out.push(("src/protocols/mod.rs".to_owned(), m));
    Ok(out)
}

fn out_stub(s: &mut String, name: &str) {
    let _ = writeln!(
        s,
        "    /// The interface table.\n    pub static INTERFACE: crate::iface::Interface =\n        crate::iface::Interface::new(c\"{name}\", 1, &[], &[]);"
    );
}
