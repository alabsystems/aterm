// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The part of the wasm binary format the generator reads and rewrites:
//! function types, imports, the function section, exports and custom
//! sections. Every other section is copied through as bytes.

use aterm_wasm_abi::descriptor::ValType;

/// A function signature, each value type as its one-byte code (`0x7f`
/// i32, `0x7e` i64, `0x7d` f32, `0x7c` f64; `0x7b` v128 and the reference
/// types a module may use internally are kept as they are).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FuncType {
    pub(crate) params: Vec<u8>,
    pub(crate) results: Vec<u8>,
}

/// The binary code of a value type.
pub(crate) fn code(v: ValType) -> u8 {
    match v {
        ValType::I32 => 0x7f,
        ValType::I64 => 0x7e,
        ValType::F32 => 0x7d,
        ValType::F64 => 0x7c,
    }
}

/// What an import or export names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Func,
    Table,
    Memory,
    Global,
    Tag,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Import {
    pub(crate) module: String,
    pub(crate) name: String,
    pub(crate) kind: Kind,
    /// For a function: its type's index.
    pub(crate) ty: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Export {
    pub(crate) name: String,
    pub(crate) kind: Kind,
    pub(crate) index: u32,
}

/// One top-level section: its id and the payload's byte range.
#[derive(Clone, Debug)]
struct Section {
    id: u8,
    start: usize,
    end: usize,
    /// A custom section's name.
    name: Option<String>,
}

/// A parsed module over its bytes.
pub(crate) struct Module<'a> {
    bytes: &'a [u8],
    sections: Vec<Section>,
    pub(crate) types: Vec<FuncType>,
    pub(crate) imports: Vec<Import>,
    /// The type index of each function the module defines.
    pub(crate) funcs: Vec<u32>,
    pub(crate) exports: Vec<Export>,
}

const MAGIC: &[u8; 8] = b"\0asm\x01\0\0\0";
const CUSTOM: u8 = 0;
const TYPE: u8 = 1;
const IMPORT: u8 = 2;
const FUNCTION: u8 = 3;
const EXPORT: u8 = 7;

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8], pos: usize) -> Self {
        Self { bytes, pos }
    }

    fn byte(&mut self) -> Result<u8, String> {
        let b = *self
            .bytes
            .get(self.pos)
            .ok_or_else(|| format!("unexpected end at byte {}", self.pos))?;
        self.pos += 1;
        Ok(b)
    }

    fn leb(&mut self, bits: u32) -> Result<u64, String> {
        let mut out = 0u64;
        let mut shift = 0;
        loop {
            let b = self.byte()?;
            if shift >= bits {
                return Err(format!("LEB128 too long at byte {}", self.pos));
            }
            out |= u64::from(b & 0x7f) << shift;
            shift += 7;
            if b & 0x80 == 0 {
                return Ok(out);
            }
        }
    }

    fn u32(&mut self) -> Result<u32, String> {
        let v = self.leb(35)?;
        u32::try_from(v).map_err(|_| format!("u32 out of range at byte {}", self.pos))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.bytes.len())
            .ok_or_else(|| format!("unexpected end at byte {}", self.pos))?;
        let s = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn name(&mut self) -> Result<String, String> {
        let n = self.u32()? as usize;
        let s = self.take(n)?;
        String::from_utf8(s.to_vec())
            .map_err(|_| format!("a name is not UTF-8 at byte {}", self.pos))
    }

    fn valtype(&mut self) -> Result<u8, String> {
        match self.byte()? {
            b @ (0x7f | 0x7e | 0x7d | 0x7c | 0x7b | 0x70 | 0x6f) => Ok(b),
            other => Err(format!(
                "value type {other:#04x} at byte {}: a typed reference (wasm GC) this reader \
                 does not parse",
                self.pos
            )),
        }
    }

    fn limits(&mut self) -> Result<(), String> {
        let flags = self.byte()?;
        self.leb(64)?;
        if flags & 1 != 0 {
            self.leb(64)?;
        }
        Ok(())
    }
}

impl<'a> Module<'a> {
    pub(crate) fn parse(bytes: &'a [u8]) -> Result<Self, String> {
        if bytes.get(..8) != Some(MAGIC.as_slice()) {
            return Err("not a wasm module (no \\0asm version 1 header)".to_string());
        }
        let mut m = Module {
            bytes,
            sections: Vec::new(),
            types: Vec::new(),
            imports: Vec::new(),
            funcs: Vec::new(),
            exports: Vec::new(),
        };
        let mut r = Reader::new(bytes, 8);
        while r.pos < bytes.len() {
            let id = r.byte()?;
            let size = r.u32()? as usize;
            let start = r.pos;
            r.take(size)?;
            let end = r.pos;
            let mut s = Reader::new(&bytes[..end], start);
            let name = match id {
                CUSTOM => Some(s.name()?),
                TYPE => {
                    m.types = Self::read_types(&mut s)?;
                    None
                }
                IMPORT => {
                    m.imports = read_imports(&mut s)?;
                    None
                }
                FUNCTION => {
                    let n = s.u32()?;
                    m.funcs = (0..n).map(|_| s.u32()).collect::<Result<_, _>>()?;
                    None
                }
                EXPORT => {
                    m.exports = read_exports(&mut s)?;
                    None
                }
                _ => None,
            };
            m.sections.push(Section {
                id,
                start,
                end,
                name,
            });
        }
        Ok(m)
    }

    fn read_types(r: &mut Reader<'_>) -> Result<Vec<FuncType>, String> {
        let n = r.u32()?;
        let mut out = Vec::with_capacity(n as usize);
        for _ in 0..n {
            if r.byte()? != 0x60 {
                return Err("a type that is not a plain function type".to_string());
            }
            let np = r.u32()?;
            let params = (0..np).map(|_| r.valtype()).collect::<Result<_, _>>()?;
            let nr = r.u32()?;
            let results = (0..nr).map(|_| r.valtype()).collect::<Result<_, _>>()?;
            out.push(FuncType { params, results });
        }
        Ok(out)
    }

    /// The bytes of each custom section called `name`, in order.
    /// The names of the module's custom sections, in order.
    pub(crate) fn custom_names(&self) -> impl Iterator<Item = &str> {
        self.sections.iter().filter_map(|s| s.name.as_deref())
    }

    pub(crate) fn custom(&self, name: &str) -> Vec<&'a [u8]> {
        self.sections
            .iter()
            .filter(|s| s.name.as_deref() == Some(name))
            .map(|s| {
                let mut r = Reader::new(self.bytes, s.start);
                // Skip the section's own name.
                let _ = r.name();
                &self.bytes[r.pos..s.end]
            })
            .collect()
    }

    /// The signature of function `index` (imports first, then defined).
    pub(crate) fn func_type(&self, index: u32) -> Option<&FuncType> {
        let imported: Vec<&Import> = self
            .imports
            .iter()
            .filter(|i| i.kind == Kind::Func)
            .collect();
        let ty = match imported.get(index as usize) {
            Some(i) => i.ty?,
            None => *self.funcs.get(index as usize - imported.len())?,
        };
        self.types.get(ty as usize)
    }

    /// The module without the custom sections named in `drop_custom` and
    /// without the exports named in `drop_exports`; every other byte as it
    /// was.
    pub(crate) fn rewrite(&self, drop_custom: &[&str], drop_exports: &[&str]) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        for s in &self.sections {
            if s.name.as_deref().is_some_and(|n| drop_custom.contains(&n)) {
                continue;
            }
            let payload = if s.id == EXPORT {
                let kept: Vec<&Export> = self
                    .exports
                    .iter()
                    .filter(|e| !drop_exports.contains(&e.name.as_str()))
                    .collect();
                let mut p = Vec::new();
                leb(&mut p, kept.len() as u32);
                for e in kept {
                    leb(&mut p, e.name.len() as u32);
                    p.extend(e.name.as_bytes());
                    p.push(match e.kind {
                        Kind::Func => 0,
                        Kind::Table => 1,
                        Kind::Memory => 2,
                        Kind::Global => 3,
                        Kind::Tag => 4,
                    });
                    leb(&mut p, e.index);
                }
                p
            } else {
                self.bytes[s.start..s.end].to_vec()
            };
            out.push(s.id);
            leb(&mut out, payload.len() as u32);
            out.extend(payload);
        }
        out
    }
}

fn kind(b: u8) -> Result<Kind, String> {
    Ok(match b {
        0 => Kind::Func,
        1 => Kind::Table,
        2 => Kind::Memory,
        3 => Kind::Global,
        4 => Kind::Tag,
        other => return Err(format!("unknown external kind {other}")),
    })
}

fn read_imports(r: &mut Reader<'_>) -> Result<Vec<Import>, String> {
    let n = r.u32()?;
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let module = r.name()?;
        let name = r.name()?;
        let k = kind(r.byte()?)?;
        let ty = match k {
            Kind::Func => Some(r.u32()?),
            Kind::Table => {
                r.byte()?;
                r.limits()?;
                None
            }
            Kind::Memory => {
                r.limits()?;
                None
            }
            Kind::Global => {
                r.byte()?;
                r.byte()?;
                None
            }
            Kind::Tag => {
                r.byte()?;
                r.u32()?;
                None
            }
        };
        out.push(Import {
            module,
            name,
            kind: k,
            ty,
        });
    }
    Ok(out)
}

fn read_exports(r: &mut Reader<'_>) -> Result<Vec<Export>, String> {
    let n = r.u32()?;
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let name = r.name()?;
        let k = kind(r.byte()?)?;
        out.push(Export {
            name,
            kind: k,
            index: r.u32()?,
        });
    }
    Ok(out)
}

/// Append `v` as unsigned LEB128.
pub(crate) fn leb(out: &mut Vec<u8>, mut v: u32) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A module built by hand: types, one import, two functions, a memory,
    /// exports and a custom section.
    pub(crate) fn sample(custom: &[(&str, &[u8])]) -> Vec<u8> {
        fn section(out: &mut Vec<u8>, id: u8, payload: &[u8]) {
            out.push(id);
            leb(out, payload.len() as u32);
            out.extend(payload);
        }
        let mut m = MAGIC.to_vec();
        // (i32, i64) -> f64 ; () -> ()
        section(&mut m, TYPE, &[2, 0x60, 2, 0x7f, 0x7e, 1, 0x7c, 0x60, 0, 0]);
        // import env.f : type 1
        section(&mut m, IMPORT, &[1, 3, b'e', b'n', b'v', 1, b'f', 0, 1]);
        // two defined functions: types 0 and 1
        section(&mut m, FUNCTION, &[2, 0, 1]);
        // memory 1
        section(&mut m, 5, &[1, 0, 1]);
        // exports: memory, "g" = func 1 (defined #0), "h" = func 2, "__data_end" global 0
        let mut e = vec![4];
        for (name, k, i) in [
            ("memory", 2u8, 0u8),
            ("g", 0, 1),
            ("h", 0, 2),
            ("__data_end", 3, 0),
        ] {
            e.push(name.len() as u8);
            e.extend(name.as_bytes());
            e.extend([k, i]);
        }
        section(&mut m, EXPORT, &e);
        for (name, bytes) in custom {
            let mut c = vec![name.len() as u8];
            c.extend(name.as_bytes());
            c.extend(*bytes);
            section(&mut m, CUSTOM, &c);
        }
        // a code section the reader passes through
        section(&mut m, 10, &[2, 2, 0, 0x0b, 2, 0, 0x0b]);
        m
    }

    #[test]
    fn the_reader_sees_types_imports_exports_and_custom_sections() {
        let bytes = sample(&[
            ("aterm_abi", b"one"),
            ("other", b"x"),
            ("aterm_abi", b"two"),
        ]);
        let m = Module::parse(&bytes).unwrap();
        assert_eq!(m.imports[0].module, "env");
        assert_eq!(m.imports[0].ty, Some(1));
        assert_eq!(m.funcs, [0, 1]);
        let names: Vec<&str> = m.exports.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["memory", "g", "h", "__data_end"]);
        assert_eq!(
            m.func_type(1),
            Some(&FuncType {
                params: vec![0x7f, 0x7e],
                results: vec![0x7c]
            })
        );
        assert_eq!(
            m.func_type(0),
            m.func_type(2),
            "the import and h share type 1"
        );
        assert_eq!(m.custom("aterm_abi"), [b"one".as_slice(), b"two"]);
    }

    #[test]
    fn a_rewrite_drops_only_what_it_names() {
        let bytes = sample(&[("aterm_abi", b"rec"), ("keep", b"k")]);
        let m = Module::parse(&bytes).unwrap();
        let out = m.rewrite(&["aterm_abi"], &["__data_end"]);
        let r = Module::parse(&out).unwrap();
        assert!(r.custom("aterm_abi").is_empty());
        assert_eq!(r.custom("keep"), [b"k".as_slice()]);
        let names: Vec<&str> = r.exports.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["memory", "g", "h"]);
        assert_eq!(r.funcs, m.funcs);
        // Nothing dropped: the same bytes.
        assert_eq!(m.rewrite(&[], &[]), bytes);
    }

    #[test]
    fn a_foreign_or_cut_file_is_refused() {
        assert!(Module::parse(b"\0asm\x02\0\0\0").is_err());
        let bytes = sample(&[]);
        assert!(Module::parse(&bytes[..bytes.len() - 2]).is_err());
    }
}
