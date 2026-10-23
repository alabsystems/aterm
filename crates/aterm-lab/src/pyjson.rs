// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Insertion-ordered JSON value with Python `json.dumps`-compatible formatting.

use std::fmt::Write as _;

/// A JSON value whose objects keep their insertion order.
#[derive(Clone, Debug, PartialEq)]
pub enum J {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A float.
    Float(f64),
    /// A string.
    Str(String),
    /// An array.
    Arr(Vec<J>),
    /// An object, in insertion order.
    Obj(Vec<(String, J)>),
}

/// How [`J::dump`] spells a value.
#[derive(Clone, Copy, Debug)]
pub struct Style {
    /// `Some(n)`: one item per line, indented `n` spaces per level.
    pub indent: Option<usize>,
    /// `","` and `":"` with no space (Python's `separators=(",", ":")`).
    pub compact: bool,
    /// Escape every non-ASCII character as `\uXXXX` (Python's default).
    pub ascii: bool,
}

/// `json.dumps(v)`.
pub const PY: Style = Style {
    indent: None,
    compact: false,
    ascii: true,
};
/// `json.dumps(v, separators=(",", ":"))`.
pub const COMPACT: Style = Style {
    indent: None,
    compact: true,
    ascii: true,
};

impl J {
    /// Parse JSON text.
    ///
    /// # Errors
    /// The text is not one JSON value.
    pub fn parse(text: &str) -> Result<J, String> {
        aterm_json::from_str::<aterm_json::Value>(text)
            .map(|v| J::from(&v))
            .map_err(|e| e.to_string())
    }

    /// Parse JSON bytes.
    ///
    /// # Errors
    /// The bytes are not one JSON value.
    pub fn parse_bytes(bytes: &[u8]) -> Result<J, String> {
        aterm_json::from_slice::<aterm_json::Value>(bytes)
            .map(|v| J::from(&v))
            .map_err(|e| e.to_string())
    }

    /// A string value.
    #[must_use]
    pub fn s(v: impl Into<String>) -> J {
        J::Str(v.into())
    }

    /// An object from pairs.
    #[must_use]
    pub fn obj<const N: usize>(pairs: [(&str, J); N]) -> J {
        J::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    /// The member `key` of an object.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&J> {
        match self {
            J::Obj(m) => m.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The text of a string value.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            J::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The elements of an array.
    #[must_use]
    pub fn as_arr(&self) -> Option<&[J]> {
        match self {
            J::Arr(a) => Some(a),
            _ => None,
        }
    }

    /// An integer (a float with no fraction counts).
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            J::Int(i) => Some(*i),
            #[allow(clippy::cast_possible_truncation)]
            J::Float(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => Some(*f as i64),
            _ => None,
        }
    }

    /// A number as a float.
    #[must_use]
    pub const fn as_f64(&self) -> Option<f64> {
        match self {
            #[allow(clippy::cast_precision_loss)]
            J::Int(i) => Some(*i as f64),
            J::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Python truthiness (`if v:`): null, false, 0, "", [] and {} are false.
    #[must_use]
    pub fn truthy(&self) -> bool {
        match self {
            J::Null => false,
            J::Bool(b) => *b,
            J::Int(i) => *i != 0,
            J::Float(f) => *f != 0.0,
            J::Str(s) => !s.is_empty(),
            J::Arr(a) => !a.is_empty(),
            J::Obj(m) => !m.is_empty(),
        }
    }

    /// Set `key` in an object (replacing it in place, else appending).
    pub fn set(&mut self, key: &str, value: J) {
        if let J::Obj(m) = self {
            if let Some(slot) = m.iter_mut().find(|(k, _)| k == key) {
                slot.1 = value;
            } else {
                m.push((key.to_string(), value));
            }
        }
    }

    /// Remove `key` from an object.
    pub fn remove(&mut self, key: &str) {
        if let J::Obj(m) = self {
            m.retain(|(k, _)| k != key);
        }
    }

    /// The member `key`, made an object first when it is not one (`setdefault(key, {})`).
    pub fn entry_obj(&mut self, key: &str) -> &mut J {
        if !matches!(self.get(key), Some(J::Obj(_))) {
            self.set(key, J::Obj(Vec::new()));
        }
        match self {
            J::Obj(m) => {
                &mut m
                    .iter_mut()
                    .rev()
                    .find(|(k, _)| k == key)
                    .expect("set above")
                    .1
            }
            _ => unreachable!("entry_obj on a non-object"),
        }
    }

    /// Spell the value.
    #[must_use]
    pub fn dump(&self, style: Style) -> String {
        let mut out = String::new();
        self.write(&mut out, style, 0);
        out
    }

    /// `json.dumps(v)`.
    #[must_use]
    pub fn py(&self) -> String {
        self.dump(PY)
    }

    fn write(&self, out: &mut String, st: Style, depth: usize) {
        let (item_sep, key_sep) = match (st.compact, st.indent) {
            (true, _) => (",", ":"),
            (false, Some(_)) => (",", ": "),
            (false, None) => (", ", ": "),
        };
        let newline = |out: &mut String, d: usize| {
            if let Some(n) = st.indent {
                out.push('\n');
                out.push_str(&" ".repeat(n * d));
            }
        };
        match self {
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Int(i) => {
                let _ = write!(out, "{i}");
            }
            J::Float(f) => out.push_str(&py_float(*f)),
            J::Str(s) => write_str(out, s, st.ascii),
            J::Arr(a) if a.is_empty() => out.push_str("[]"),
            J::Arr(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push_str(item_sep);
                    }
                    newline(out, depth + 1);
                    v.write(out, st, depth + 1);
                }
                newline(out, depth);
                out.push(']');
            }
            J::Obj(m) if m.is_empty() => out.push_str("{}"),
            J::Obj(m) => {
                out.push('{');
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push_str(item_sep);
                    }
                    newline(out, depth + 1);
                    write_str(out, k, st.ascii);
                    out.push_str(key_sep);
                    v.write(out, st, depth + 1);
                }
                newline(out, depth);
                out.push('}');
            }
        }
    }
}

impl From<&aterm_json::Value> for J {
    fn from(v: &aterm_json::Value) -> Self {
        use aterm_json::Value as V;
        match v {
            V::Null => J::Null,
            V::Bool(b) => J::Bool(*b),
            V::Number(n) => match (n.as_i64(), n.as_f64()) {
                (Some(i), _) => J::Int(i),
                (None, Some(f)) => J::Float(f),
                (None, None) => J::Null,
            },
            V::String(s) => J::Str(s.clone()),
            V::Array(a) => J::Arr(a.iter().map(J::from).collect()),
            V::Object(m) => J::Obj(m.iter().map(|(k, v)| (k.clone(), J::from(v))).collect()),
        }
    }
}

impl From<&str> for J {
    fn from(s: &str) -> Self {
        J::Str(s.to_string())
    }
}

impl From<String> for J {
    fn from(s: String) -> Self {
        J::Str(s)
    }
}

impl From<i64> for J {
    fn from(i: i64) -> Self {
        J::Int(i)
    }
}

impl From<f64> for J {
    fn from(f: f64) -> Self {
        J::Float(f)
    }
}

impl From<bool> for J {
    fn from(b: bool) -> Self {
        J::Bool(b)
    }
}

impl<T: Into<J>> From<Option<T>> for J {
    fn from(o: Option<T>) -> Self {
        o.map_or(J::Null, Into::into)
    }
}

impl<T: Into<J>> From<Vec<T>> for J {
    fn from(v: Vec<T>) -> Self {
        J::Arr(v.into_iter().map(Into::into).collect())
    }
}

/// Formats `f` like Python's float `repr`.
#[must_use]
pub fn py_float(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        };
    }
    let a = f.abs();
    if a != 0.0 && !(1e-4..1e16).contains(&a) {
        let s = format!("{f:e}");
        let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
        let (sign, digits) = exp.strip_prefix('-').map_or(("+", exp), |d| ("-", d));
        return format!("{mant}e{sign}{digits:0>2}");
    }
    let s = format!("{f}");
    if s.contains('.') { s } else { format!("{s}.0") }
}

fn write_str(out: &mut String, s: &str, ascii: bool) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c if ascii && (c as u32) > 0x7e && c != '\u{7f}' => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Recursively merges `from` into `into`, removing keys set to `null`.
pub fn merge(into: &mut J, from: &J) {
    let J::Obj(pairs) = from else { return };
    for (k, v) in pairs {
        match v {
            J::Null => into.remove(k),
            J::Obj(_) if matches!(into.get(k), Some(J::Obj(_))) => merge(into.entry_obj(k), v),
            _ => into.set(k, v.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spells_as_python_json_dumps_does() {
        let v = J::obj([
            ("a", J::Int(1)),
            ("b", J::Arr(vec![J::s("x"), J::Null])),
            ("c", J::obj([])),
        ]);
        assert_eq!(v.py(), r#"{"a": 1, "b": ["x", null], "c": {}}"#);
        assert_eq!(v.dump(COMPACT), r#"{"a":1,"b":["x",null],"c":{}}"#);
        let pretty = v.dump(Style {
            indent: Some(2),
            compact: false,
            ascii: true,
        });
        assert_eq!(
            pretty,
            "{\n  \"a\": 1,\n  \"b\": [\n    \"x\",\n    null\n  ],\n  \"c\": {}\n}"
        );
        assert_eq!(J::s("é❯").py(), r#""\u00e9\u276f""#);
        assert_eq!(
            J::s("é").dump(Style {
                indent: None,
                compact: false,
                ascii: false
            }),
            "\"é\""
        );
    }

    #[test]
    fn floats_read_as_python_repr() {
        assert_eq!(py_float(2.0), "2.0");
        assert_eq!(py_float(1_792_091_818.268_117), "1792091818.268117");
        assert_eq!(py_float(0.414), "0.414");
        assert_eq!(py_float(1e16), "1e+16");
        assert_eq!(py_float(1.5e-5), "1.5e-05");
    }

    #[test]
    fn merge_removes_on_null_merges_objects_and_replaces_lists() {
        let mut base =
            J::parse(r#"{"env":{"A":"1"},"permissions":{"allow":["x"]},"apiKeyHelper":"/k"}"#)
                .unwrap();
        let patch =
            J::parse(r#"{"apiKeyHelper":null,"env":{"B":"2"},"permissions":{"allow":["y"]}}"#)
                .unwrap();
        merge(&mut base, &patch);
        assert_eq!(base.get("apiKeyHelper"), None);
        assert_eq!(base.get("env").unwrap().py(), r#"{"A": "1", "B": "2"}"#);
        assert_eq!(base.get("permissions").unwrap().py(), r#"{"allow": ["y"]}"#);
    }
}
