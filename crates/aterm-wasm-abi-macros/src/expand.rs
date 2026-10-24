// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Macro expansion for `#[export]` functions, methods, classes, and `aterm_abi` descriptor records.

use crate::parse::{Error, Receiver, Sig};
use crate::tt::{Tt, byte_lit, str_lit, to_source};

const RT: &str = "::aterm_wasm_abi::__rt";

/// Record kinds (`aterm_wasm_abi::descriptor::kind`).
pub mod kind {
    /// A class.
    pub const CLASS: u8 = 1;
    /// A free function.
    pub const FUNCTION: u8 = 2;
    /// A method.
    pub const METHOD: u8 = 3;
    /// A static method.
    pub const STATIC: u8 = 4;
    /// A getter.
    pub const GETTER: u8 = 5;
    /// A constructor.
    pub const CONSTRUCTOR: u8 = 6;

    /// Return `true` when `k` is one of the 6 valid descriptor record kinds (`1..=6`).
    #[must_use]
    pub const fn is_valid(k: u8) -> bool {
        k >= CLASS && k <= CONSTRUCTOR
    }

    /// Return `true` when `k` is a class member kind.
    #[must_use]
    pub const fn is_member(k: u8) -> bool {
        matches!(k, METHOD | STATIC | GETTER | CONSTRUCTOR)
    }

    /// Encodes the four descriptor header string lengths into 16 little-endian bytes.
    #[must_use]
    pub const fn encode_record_lengths_le(
        class_len: u32,
        js_name_len: u32,
        symbol_len: u32,
        docs_len: u32,
    ) -> [u8; 16] {
        [
            class_len as u8,
            (class_len >> 8) as u8,
            (class_len >> 16) as u8,
            (class_len >> 24) as u8,
            js_name_len as u8,
            (js_name_len >> 8) as u8,
            (js_name_len >> 16) as u8,
            (js_name_len >> 24) as u8,
            symbol_len as u8,
            (symbol_len >> 8) as u8,
            (symbol_len >> 16) as u8,
            (symbol_len >> 24) as u8,
            docs_len as u8,
            (docs_len >> 8) as u8,
            (docs_len >> 16) as u8,
            (docs_len >> 24) as u8,
        ]
    }

    /// Decode 16 little-endian bytes into the four descriptor header string lengths.
    #[must_use]
    pub const fn decode_record_lengths_le(bytes: [u8; 16]) -> (u32, u32, u32, u32) {
        let c = (bytes[0] as u32)
            | ((bytes[1] as u32) << 8)
            | ((bytes[2] as u32) << 16)
            | ((bytes[3] as u32) << 24);
        let j = (bytes[4] as u32)
            | ((bytes[5] as u32) << 8)
            | ((bytes[6] as u32) << 16)
            | ((bytes[7] as u32) << 24);
        let s = (bytes[8] as u32)
            | ((bytes[9] as u32) << 8)
            | ((bytes[10] as u32) << 16)
            | ((bytes[11] as u32) << 24);
        let d = (bytes[12] as u32)
            | ((bytes[13] as u32) << 8)
            | ((bytes[14] as u32) << 16)
            | ((bytes[15] as u32) << 24);
        (c, j, s, d)
    }
}

const SECTION: &str = "aterm_abi";

const WASM: &str = "all(target_arch = \"wasm32\", target_os = \"unknown\")";

/// One export to write.
pub struct Export<'a> {
    /// [`kind`].
    pub kind: u8,
    /// For a member: the class's type as written, and its JS name.
    pub class: Option<(&'a [Tt], &'a str)>,
    /// The JS name.
    pub js_name: String,
    /// The wasm export.
    pub symbol: String,
    /// The doc comment.
    pub docs: String,
    /// The signature.
    pub sig: &'a Sig,
}

fn enc_str(out: &mut Vec<u8>, s: &str) {
    out.extend((s.len() as u32).to_le_bytes());
    out.extend(s.as_bytes());
}

/// The bytes of a record's fixed head.
pub fn head(kind: u8, class: &str, js_name: &str, symbol: &str, docs: &str) -> Vec<u8> {
    debug_assert!(kind::is_valid(kind));
    debug_assert!(!kind::is_member(kind) || !class.is_empty());
    let lens = kind::encode_record_lengths_le(
        class.len() as u32,
        js_name.len() as u32,
        symbol.len() as u32,
        docs.len() as u32,
    );
    let _ = kind::decode_record_lengths_le(lens);
    let mut out = vec![kind];
    for s in [class, js_name, symbol, docs] {
        enc_str(&mut out, s);
    }
    out
}

fn replace_self(tts: &[Tt], with: &[Tt]) -> Vec<Tt> {
    let mut out = Vec::with_capacity(tts.len());
    for t in tts {
        match t {
            Tt::Ident(i) if i == "Self" => out.extend(with.iter().cloned()),
            Tt::Group(d, inner) => out.push(Tt::Group(*d, replace_self(inner, with))),
            t => out.push(t.clone()),
        }
    }
    out
}

fn record(parts: &[String]) -> String {
    format!(
        "#[cfg({WASM})]\n\
         const __ATERM_ABI_PARTS: &[{RT}::Part] = &[{parts}];\n\
         #[cfg({WASM})]\n\
         #[unsafe(link_section = {section})]\n\
         static __ATERM_ABI_RECORD: [u8; {RT}::record_len(__ATERM_ABI_PARTS)] = \
         {RT}::record(__ATERM_ABI_PARTS);\n",
        parts = parts.join(", "),
        section = str_lit(SECTION),
    )
}

fn bytes_part(bytes: &[u8]) -> String {
    format!("{RT}::Part::Bytes({})", byte_lit(bytes))
}

fn ty_part(ty: &str) -> String {
    format!("{RT}::Part::Ty(<{ty} as {RT}::Describe>::DESC)")
}

/// Generates the wasm export function and descriptor record for `e`.
/// # Errors
/// More than 255 parameters, or a receiver where the kind takes none.
pub fn export(e: &Export<'_>) -> Result<String, Error> {
    let sig = e.sig;
    let class_ty: Vec<Tt> = e.class.map(|(t, _)| t.to_vec()).unwrap_or_default();
    let fix = |tts: &[Tt]| to_source(&replace_self(tts, &class_ty));
    let n = u8::try_from(sig.params.len())
        .map_err(|_| Error(format!("`{}`: more than 255 parameters", sig.name)))?;
    let receiver = match sig.receiver {
        Receiver::None => 0u8,
        Receiver::Shared => 1,
        Receiver::Exclusive => 2,
    };

    let mut head = head(
        e.kind,
        e.class.map_or("", |(_, name)| name),
        &e.js_name,
        &e.symbol,
        &e.docs,
    );
    head.push(receiver);
    head.push(n);
    let mut parts = vec![bytes_part(&head)];

    let mut words = Vec::new();
    let mut lets = Vec::new();
    let mut args = Vec::new();
    if sig.receiver != Receiver::None {
        words.push("__this: usize".to_string());
    }
    for (i, p) in sig.params.iter().enumerate() {
        let ty = fix(&p.ty);
        let (tr, conv) = if p.by_ref {
            ("RefFromAbi", "ref_from_abi")
        } else {
            ("FromAbi", "from_abi")
        };
        let mut name = Vec::new();
        enc_str(&mut name, &p.name);
        parts.push(bytes_part(&name));
        parts.push(ty_part(&ty));
        words.push(format!(
            "__p{i}_0: <{ty} as {RT}::{tr}>::W0, __p{i}_1: <{ty} as {RT}::{tr}>::W1"
        ));
        lets.push(format!(
            "// SAFETY: the glue lowers this argument as its type's `{tr}` contract says \
             (aterm_wasm_abi::marshal).\n\
             let __p{i} = unsafe {{ <{ty} as {RT}::{tr}>::{conv}(__p{i}_0, __p{i}_1) }}?;\n"
        ));
        args.push(if p.by_ref {
            format!("&*__p{i}")
        } else {
            format!("__p{i}")
        });
    }
    let ret = sig.ret.as_deref().map_or_else(|| "()".to_string(), fix);
    parts.push(ty_part(&ret));

    let callee = match e.class {
        Some((ty, _)) => format!("<{}>::{}", to_source(ty), sig.name),
        None => sig.name.clone(),
    };
    let this = match (sig.receiver, e.class) {
        (Receiver::None, _) => String::new(),
        (_, None) => {
            return Err(Error(format!(
                "`{}` takes `self`: export a method through `#[export]` on its `impl`",
                sig.name
            )));
        }
        (r, Some((ty, _))) => {
            let (borrow, binding, reborrow) = if r == Receiver::Exclusive {
                ("borrow_mut", "mut __this", "&mut *__this")
            } else {
                ("borrow", "__this", "&*__this")
            };
            args.insert(0, reborrow.to_string());
            format!(
                "// SAFETY: the glue passes 0 or a handle `into_raw` made, of any class \
                 (aterm_wasm_abi::handle).\n\
                 let {binding} = unsafe {{ {RT}::{borrow}::<{ty}>(__this) }}?;\n",
                ty = to_source(ty)
            )
        }
    };

    Ok(format!(
        "#[allow(\n\
             clippy::redundant_closure_call,\n\
             reason = \"the closure is what each conversion's `?` returns from\"\n\
         )]\n\
         #[unsafe(export_name = {symbol})]\n\
         extern \"C-unwind\" fn __aterm_abi_export({words}) -> <{ret} as {RT}::IntoAbi>::Ret {{\n\
             {RT}::returned((|| {{\n\
                 {lets}{this}{RT}::finish::<{ret}>({callee}({args}))\n\
             }})())\n\
         }}\n\
         {record}",
        symbol = str_lit(&e.symbol),
        words = words.join(", "),
        lets = lets.concat(),
        args = args.join(", "),
        record = record(&parts),
    ))
}

/// Generates trait impls.
pub fn class(name: &str, docs: &str) -> String {
    let symbol = free_symbol(name);
    format!(
        "{RT_ROOT}::__class!({name});\n\
         const _: () = {{\n\
             #[unsafe(export_name = {sym})]\n\
             extern \"C-unwind\" fn __aterm_abi_free(ptr: usize, from_finalizer: u32) {{\n\
                 // SAFETY: the glue passes a handle it got from `into_raw`, or 0 after a \
                 free (aterm_wasm_abi::handle).\n\
                 unsafe {{ {RT}::free_export::<{name}>(ptr, from_finalizer) }}\n\
             }}\n\
             {record}\
         }};\n",
        RT_ROOT = "::aterm_wasm_abi",
        sym = str_lit(&symbol),
        record = record(&[bytes_part(&head(kind::CLASS, name, name, &symbol, docs))]),
    )
}

/// A class's free export.
pub fn free_symbol(class: &str) -> String {
    format!("__aterm_free_{}", class.to_lowercase())
}

/// A member's export.
pub fn member_symbol(class: &str, name: &str) -> String {
    format!("{}_{name}", class.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{Item, item};
    use crate::tt::lex;
    use aterm_wasm_abi::descriptor as d;

    #[test]
    fn the_record_constants_are_the_runtimes() {
        assert_eq!(
            [
                kind::CLASS,
                kind::FUNCTION,
                kind::METHOD,
                kind::STATIC,
                kind::GETTER,
                kind::CONSTRUCTOR
            ],
            [
                d::kind::CLASS,
                d::kind::FUNCTION,
                d::kind::METHOD,
                d::kind::STATIC,
                d::kind::GETTER,
                d::kind::CONSTRUCTOR
            ]
        );
        assert_eq!(SECTION, d::SECTION);
    }

    #[test]
    fn a_written_head_decodes_with_the_runtimes_reader() {
        let mut body = head(
            kind::METHOD,
            "Term",
            "encode",
            "term_encode",
            " Docs —\n two.",
        );
        body.extend([2u8, 2]);
        enc_str(&mut body, "key");
        body.extend(<str as aterm_wasm_abi::__rt::Describe>::DESC.as_bytes());
        enc_str(&mut body, "n");
        body.extend(<Option<u64> as aterm_wasm_abi::__rt::Describe>::DESC.as_bytes());
        body.extend(<Result<Vec<u8>, String> as aterm_wasm_abi::__rt::Describe>::DESC.as_bytes());
        let mut rec = d::MAGIC.to_vec();
        rec.push(d::VERSION);
        rec.extend((body.len() as u32).to_le_bytes());
        rec.extend(body);
        let r = &d::decode(&rec).unwrap()[0];
        assert_eq!(
            (
                r.kind,
                r.class.as_str(),
                r.js_name.as_str(),
                r.symbol.as_str()
            ),
            (d::kind::METHOD, "Term", "encode", "term_encode")
        );
        assert_eq!(r.docs, " Docs —\n two.");
        assert_eq!(r.receiver, d::receiver::EXCLUSIVE);
        assert_eq!(
            r.params,
            [
                ("key".to_string(), d::Ty::Str),
                ("n".to_string(), d::Ty::Option(Box::new(d::Ty::U64)))
            ]
        );
        assert_eq!(
            r.ret,
            d::Ty::Result(
                Box::new(d::Ty::Vec(Box::new(d::Ty::U8))),
                Box::new(d::Ty::String)
            )
        );
    }

    fn sig(src: &str) -> Sig {
        match item(&lex(src)).unwrap() {
            Item::Fn { sig, .. } => sig,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_method_expands_to_a_borrowing_export() {
        let s =
            sig("pub fn find(&mut self, q: &str, n: Option<u32>) -> Result<Self, String> { x }");
        let ty = lex("Term");
        let out = export(&Export {
            kind: kind::METHOD,
            class: Some((&ty, "Term")),
            js_name: "find".into(),
            symbol: member_symbol("Term", "find"),
            docs: String::new(),
            sig: &s,
        })
        .unwrap();
        let tts = lex(&out);
        assert!(!tts.is_empty());
        assert!(out.contains("export_name = \"term_find\""));
        assert!(out.contains("borrow_mut::<Term>(__this)"));
        assert!(out.contains("<Term>::find(&mut *__this, &*__p0, __p1)"));
        assert!(out.contains("<str as ::aterm_wasm_abi::__rt::RefFromAbi>"));
        assert!(out.contains("Result<Term, String>") || out.contains("Result < Term , String >"));
        assert!(!out.contains("Self"));

        let shared = sig("pub fn len(&self) -> u32 { 0 }");
        let out = export(&Export {
            kind: kind::GETTER,
            class: Some((&ty, "Term")),
            js_name: "len".into(),
            symbol: member_symbol("Term", "len"),
            docs: String::new(),
            sig: &shared,
        })
        .unwrap();
        assert!(out.contains("let __this = unsafe { ::aterm_wasm_abi::__rt::borrow::<Term>"));

        let free = sig("pub fn method(&self) {}");
        assert!(
            export(&Export {
                kind: kind::FUNCTION,
                class: None,
                js_name: "method".into(),
                symbol: "method".into(),
                docs: String::new(),
                sig: &free,
            })
            .is_err(),
            "a free function cannot take self"
        );
    }

    #[test]
    fn a_class_expands_to_impls_a_free_export_and_a_record() {
        let out = class("LinkHit", " A link.");
        assert!(!lex(&out).is_empty());
        assert!(out.contains("__class!(LinkHit)"));
        assert!(out.contains("export_name = \"__aterm_free_linkhit\""));
        assert!(out.contains("free_export::<LinkHit>(ptr, from_finalizer)"));
    }
}
